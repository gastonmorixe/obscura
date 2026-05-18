//! The single piece of glue that turns a loaded `Bundle` into the JS preload
//! script Obscura injects on each navigation.
//!
//! Architecture (the simple-but-it-works flavour):
//!
//! All extension code (the `chrome` shim, the background scripts, and
//! every content script the bg later injects) runs in the **same V8
//! realm as the page** — the one Obscura already creates per-`Page` in
//! `obscura-js`. We don't spin up a second isolate for the "background"
//! world. Tradeoffs:
//!
//! - Pro: zero V8/tokio re-plumbing; we lean on `execute_preload_script`
//!   which is already wired through `Page.addScriptToEvaluateOnNewDocument`.
//! - Pro: extension JS can directly mutate the page DOM via the same
//!   `document` global content scripts expect.
//! - Con: the extension's background scripts re-run on every nav.
//!   Acceptable because their bootstrap is in the tens of milliseconds
//!   and all per-session state we care about (storage.local) is
//!   materialised in the host shim, not in the bg realm's closures.
//! - Con: extension code shares its identifier namespace with page JS.
//!   The page script could fingerprint us by reading `globalThis.chrome`.
//!   For an automation tool that's acceptable; we're not trying to
//!   masquerade as "no extension installed".
//!
//! The build_preload_script() output is a single concatenated string:
//!
//!   1. `__obscura_ext_bundle` registry: a JSON-encoded map of every
//!      script file in the bundle, keyed by manifest-relative path.
//!   2. The chrome/browser global with ~50 API methods covering the
//!      surface real-world content-script extensions touch on bringup.
//!      Some methods carry real semantics (storage, executeScript
//!      dispatching to the file registry, tabs/runtime message bus);
//!      most are graceful no-ops.
//!   3. Concatenated content of every script in
//!      `manifest.background.scripts` (MV2) or just `service_worker` (MV3).
//!   4. Trigger: a single synthesised `tabs.onUpdated` event for the
//!      current page, which is the canonical hook point extensions
//!      register their per-page work on.
//!
//! The bg listener then calls `tabs.executeScript`/`scripting.executeScript`
//! which our shim implements by looking the requested files up in the
//! registry and `eval`-ing them. Since everything is the same realm,
//! `chrome.runtime.onMessage` listeners registered by content scripts
//! observe `tabs.sendMessage` calls from bg directly.

use std::sync::Arc;

use serde_json::Value;
use url::Url;

use crate::bundle::Bundle;
use crate::host_match::MatchPattern;
use crate::state::ExtensionState;

pub struct ExtensionRuntime {
    pub bundle: Arc<Bundle>,
    pub state: Arc<ExtensionState>,
    // Pre-compiled match patterns from manifest.host_patterns, used to
    // decide whether to inject for a given URL.
    host_patterns: Vec<MatchPattern>,
}

impl ExtensionRuntime {
    pub fn new(bundle: Arc<Bundle>) -> Self {
        let host_patterns = bundle
            .manifest
            .host_patterns
            .iter()
            .filter_map(|p| MatchPattern::parse(p))
            .collect();
        Self {
            bundle,
            state: Arc::new(ExtensionState::new()),
            host_patterns,
        }
    }

    /// Does this extension want to run on the given URL? Used by
    /// `obscura-browser` to short-circuit injection on out-of-scope hosts.
    pub fn matches_url(&self, url: &str) -> bool {
        let parsed = match Url::parse(url) {
            Ok(u) => u,
            Err(_) => return false,
        };
        self.host_patterns.iter().any(|p| p.matches_url(&parsed))
    }

    /// Build the JS preload script to inject into the current page's V8
    /// realm. Idempotent — same input URL produces same string. Caller
    /// is responsible for guarding against double-injection (Obscura's
    /// `Page::init_js` rebuilds the realm on every nav, so a single call
    /// per nav is correct).
    pub fn build_preload_script(&self, url: &str) -> String {
        // Build the script files registry. Only JS files are exposed —
        // these are the ones the extension might pass to
        // `chrome.tabs.executeScript` / `chrome.scripting.executeScript`
        // for content-script injection. Excluding `.html` / `.png` /
        // `.css` / `.json` keeps the embedded blob lean.
        let mut registry: serde_json::Map<String, Value> = serde_json::Map::new();
        for (path, bytes) in &self.bundle.files {
            if !path.ends_with(".js") {
                continue;
            }
            // Skip background scripts; they get inlined directly below.
            if self.bundle.manifest.background_scripts.iter().any(|s| s == path) {
                continue;
            }
            let text = String::from_utf8_lossy(bytes).into_owned();
            registry.insert(path.clone(), Value::String(text));
        }
        let registry_json = serde_json::to_string(&registry).unwrap_or_else(|_| "{}".into());

        // Manifest as JSON for chrome.runtime.getManifest. We send a
        // hand-built subset rather than re-serialising the original
        // because real-world extensions typically only inspect a handful
        // of fields and the original can carry up to ~1 MB of host
        // patterns we don't need to round-trip through V8.
        //
        // Forwarded carefully:
        //
        // - `update_url` / `browser_specific_settings.gecko.update_url`:
        //   some extensions use presence of an update URL as the
        //   "self-hosted vs store-installed" signal and gate behaviour
        //   on it (e.g. "only run on signed releases"). Pass both
        //   variants through so those gates don't trip on side-loaded
        //   bundles.
        // - `key`: intentionally NOT forwarded. Extensions use
        //   presence-of-key as the "is this a Chrome extension?" signal,
        //   and MV2 builds in real Firefox don't have it. Leaving it out
        //   keeps the Firefox-shaped code path active even when loading
        //   what was originally a Chrome bundle.
        let mut manifest_obj = serde_json::json!({
            "manifest_version": self.bundle.manifest.manifest_version,
            "name": self.bundle.manifest.name,
            "version": self.bundle.manifest.version,
            "permissions": self.bundle.manifest.permissions,
        });
        // Re-parse the raw manifest.json once to forward the
        // update_url-style fields verbatim.
        if let Some(raw) = self.bundle.get_text("manifest.json") {
            if let Ok(raw_v) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(u) = raw_v.get("update_url") {
                    manifest_obj["update_url"] = u.clone();
                }
                if let Some(b) = raw_v.get("browser_specific_settings") {
                    manifest_obj["browser_specific_settings"] = b.clone();
                }
            }
        }
        let manifest_json = serde_json::to_string(&manifest_obj).unwrap();

        let extension_id_json = serde_json::to_string(&self.bundle.manifest.extension_id).unwrap();
        let url_json = serde_json::to_string(url).unwrap();
        let manifest_version = self.bundle.manifest.manifest_version;

        // Build the background script chunk.
        //
        // Right before the final background script runs, we interleave a
        // short "auto-enable" snippet. Many real-world extensions ship a
        // table of per-site configuration in an earlier background
        // script and then, in the next one, read `chrome.storage.local`
        // to decide which of those sites the user has actually opted
        // into. With no options-page UI to flip those toggles, the
        // extension would start up in its "fresh install, nothing
        // enabled" mode — and never act.
        //
        // The heuristic: if a `defaultSites` global (an
        // `{[displayName: string]: {domain: string, …}}` map) is
        // visible in the IIFE scope after the preceding background
        // scripts evaluate, project it into
        // `chrome.storage.local._data.sites` so the next background
        // script reads "everything bundled is enabled". Extensions that
        // don't use that schema are unaffected — the snippet's
        // `typeof defaultSites === 'object'` gate skips silently.
        //
        // We have to reference the bare `defaultSites` binding rather
        // than `globalThis.defaultSites`: the whole bundle runs INSIDE
        // the bootstrap IIFE, so a `var defaultSites = …` at a bundle
        // script's top level becomes function-local to the IIFE, not a
        // global.
        let mut bg_src = String::new();
        let bg_scripts = &self.bundle.manifest.background_scripts;
        for (i, bg) in bg_scripts.iter().enumerate() {
            if i == bg_scripts.len().saturating_sub(1) {
                bg_src.push_str(
                    r#"
// === obscura-ext: auto-enable bundled sites in storage.local ===
try {
  if (typeof defaultSites === 'object' && defaultSites) {
    const _seed_sites = {};
    let _seed_count = 0;
    for (const _name of Object.keys(defaultSites)) {
      const _info = defaultSites[_name];
      if (_info && _info.domain && !/^(#options_|###$)/.test(_info.domain)) {
        _seed_sites[_name] = _info.domain;
        _seed_count++;
      }
    }
    globalThis.chrome.storage.local._data.sites = _seed_sites;
    globalThis.chrome.storage.local._data.optIn = false;
    globalThis.chrome.storage.local._data.optInFetch = false;
    globalThis.chrome.storage.local._data.optInUpdate = false;
    console.log("obscura-ext: auto-enabled " + _seed_count + " bundled sites");
  }
} catch (e) { console.error("obscura-ext: auto-enable failed:", e && e.stack || e); }
"#,
                );
            }
            if let Some(text) = self.bundle.get_text(bg) {
                bg_src.push_str(&format!("\n// === bundle: {bg} ===\n"));
                bg_src.push_str(&text);
                bg_src.push('\n');
            } else {
                tracing::warn!("extension background script not found in bundle: {bg}");
            }
        }

        // The shim itself. Generous-ish but stays under 25 KB. Comments
        // are heavy because future Obscura contributors will be
        // debugging extension interactions here whenever a target site
        // changes shape and an extension's content scripts stop working.
        let shim = include_str!("../js/chrome_shim.js");

        // Final wrapper: an IIFE so vars don't leak; ends by running
        // background scripts. The bootstrap stages emit info-level
        // console messages so a tracing-enabled run (`RUST_LOG=obscura::console=info`)
        // shows exactly which stage broke the chain when a target site
        // stops working. The opt-in `__obscura_ext_trace_timers` flag
        // adds per-setTimeout / per-drain logging — useful when chasing
        // listener-registration ordering bugs, off by default to keep
        // info-level output readable.
        format!(
            r#"
(function obscuraExtensionBootstrap() {{
  if (globalThis.__obscura_ext_loaded) return;
  globalThis.__obscura_ext_loaded = true;
  globalThis.__obscura_ext_bundle = {registry_json};
  globalThis.__obscura_ext_manifest = {manifest_json};
  globalThis.__obscura_ext_id = {extension_id_json};
  globalThis.__obscura_ext_url = {url_json};
  globalThis.__obscura_ext_mv = {manifest_version};
  console.log("obscura-ext: preload starting (" + Object.keys(globalThis.__obscura_ext_bundle).length + " bundled scripts)");
  try {{
    {shim}
  }} catch (e) {{
    console.error("obscura-ext: chrome shim failed:", e && e.stack || e);
    return;
  }}
  try {{
    {bg_src}
  }} catch (e) {{
    console.error("obscura-ext: background scripts failed:", e && e.stack || e);
  }}
  // Synthesise `tabs.onUpdated` for the current page so the extension's
  // background listener fires its per-page hook. That listener
  // typically queues a setTimeout chain to inject content scripts and
  // dispatch a background→content-script config message; we then
  // drain that
  // queue synchronously below — see chrome_shim.js's setTimeout
  // override for the rationale (real-browser async timing doesn't
  // survive Obscura's single-shot preload, so we materialise it as a
  // synchronous queue we can flush before returning).
  try {{
    globalThis.__obscura_ext_fire_loaded(globalThis.__obscura_ext_url);
    if (typeof globalThis.__obscura_ext_drain_timers === "function") {{
      const drained = globalThis.__obscura_ext_drain_timers(50);
      console.log("obscura-ext: drained " + drained + " timers, onUpdated listeners=" + globalThis.__obscura_ext_onupdated_count());
    }}
  }} catch (e) {{
    console.error("obscura-ext: fire/drain failed:", e && e.stack || e);
  }}
}})();
"#
        )
    }
}
