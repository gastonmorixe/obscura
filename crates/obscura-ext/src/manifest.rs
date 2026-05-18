//! WebExtension manifest parsing.
//!
//! Handles both MV2 (Firefox-style, persistent background-script list,
//! host patterns inlined in `permissions`) and MV3 (Chrome-style,
//! service-worker background, hosts in `host_permissions`). Both load
//! paths are supported because real-world extensions commonly ship a
//! single JS codebase that branches on `manifest_version` at runtime;
//! a host that only supported one flavour would force the user to pick
//! which port to install.

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("manifest.json not found in extension bundle")]
    Missing,
    #[error("invalid manifest.json: {0}")]
    Invalid(#[from] serde_json::Error),
    #[error("unsupported manifest_version: {0}")]
    Unsupported(u32),
}

/// Normalised view of a WebExtension manifest.
///
/// Only the fields Obscura's host actually consults are extracted; everything
/// else (icons, action UI, options pages) is intentionally dropped so a future
/// schema change to a peripheral field doesn't break the loader.
#[derive(Debug, Clone)]
pub struct ExtensionManifest {
    pub name: String,
    pub version: String,
    pub manifest_version: u32,
    /// Background script files, in load order. For MV2 these come from
    /// `background.scripts`; for MV3 we take the single `service_worker`.
    pub background_scripts: Vec<String>,
    /// Static content-script entries (rarely populated in modern
    /// extensions, which typically inject everything dynamically from
    /// the background script via `tabs.executeScript` /
    /// `scripting.executeScript`).
    pub content_scripts: Vec<ContentScriptEntry>,
    /// Host match patterns (`*://*.example.com/*`). MV2 mixes them into
    /// `permissions`; MV3 puts them in `host_permissions`. We merge.
    pub host_patterns: Vec<String>,
    /// Non-host permission names (`cookies`, `storage`, `webRequest`, …).
    pub permissions: Vec<String>,
    /// MV3 has a stable extension id derived from the public key; MV2 has
    /// `browser_specific_settings.gecko.id`. Fallback: synthesised from name.
    pub extension_id: String,
}

#[derive(Debug, Clone)]
pub struct ContentScriptEntry {
    pub matches: Vec<String>,
    pub js: Vec<String>,
    pub run_at: String,
    pub all_frames: bool,
}

#[derive(Deserialize)]
struct RawManifest {
    #[serde(default)]
    manifest_version: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    background: Option<RawBackground>,
    #[serde(default)]
    content_scripts: Vec<RawContentScript>,
    #[serde(default)]
    permissions: Vec<String>,
    #[serde(default)]
    host_permissions: Vec<String>,
    #[serde(default)]
    browser_specific_settings: Option<RawBrowserSpecific>,
    #[serde(default)]
    key: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawBackground {
    // Order matters with `untagged`: try the more specific variant
    // (`service_worker` is required) before the MV2 form whose only
    // member is `#[serde(default)]` and would silently match anything.
    Mv3 { service_worker: String },
    Mv2 {
        #[serde(default)]
        scripts: Vec<String>,
    },
}

#[derive(Deserialize)]
struct RawContentScript {
    #[serde(default)]
    matches: Vec<String>,
    #[serde(default)]
    js: Vec<String>,
    #[serde(default = "default_run_at")]
    run_at: String,
    #[serde(default)]
    all_frames: bool,
}

fn default_run_at() -> String {
    "document_idle".into()
}

#[derive(Deserialize)]
struct RawBrowserSpecific {
    gecko: Option<RawGecko>,
}

#[derive(Deserialize)]
struct RawGecko {
    id: Option<String>,
}

impl ExtensionManifest {
    pub fn parse(raw_json: &str) -> Result<Self, ManifestError> {
        let raw: RawManifest = serde_json::from_str(raw_json)?;
        if !matches!(raw.manifest_version, 2 | 3) {
            return Err(ManifestError::Unsupported(raw.manifest_version));
        }

        let background_scripts = match raw.background {
            Some(RawBackground::Mv2 { scripts }) => scripts,
            Some(RawBackground::Mv3 { service_worker }) => vec![service_worker],
            None => Vec::new(),
        };

        // MV2 inlines hosts into `permissions`; split them out.
        let mut host_patterns: Vec<String> = raw.host_permissions.clone();
        let mut permissions: Vec<String> = Vec::new();
        for p in raw.permissions {
            if is_host_pattern(&p) {
                host_patterns.push(p);
            } else {
                permissions.push(p);
            }
        }

        let content_scripts = raw
            .content_scripts
            .into_iter()
            .map(|c| ContentScriptEntry {
                matches: c.matches,
                js: c.js,
                run_at: c.run_at,
                all_frames: c.all_frames,
            })
            .collect();

        let extension_id = raw
            .browser_specific_settings
            .as_ref()
            .and_then(|b| b.gecko.as_ref())
            .and_then(|g| g.id.clone())
            .or_else(|| {
                // MV3: synthesize a stable id from the key. Real Chrome derives a
                // 32-char a-p hash; for our purposes any stable string works.
                raw.key.as_ref().map(|_| format!("mv3@{}", &raw.name))
            })
            .unwrap_or_else(|| format!("unsigned@{}", raw.name));

        Ok(ExtensionManifest {
            manifest_version: raw.manifest_version,
            name: raw.name,
            version: raw.version,
            background_scripts,
            content_scripts,
            host_patterns,
            permissions,
            extension_id,
        })
    }

    /// MV3 service-worker model adds keepalive semantics we don't emulate;
    /// the loader uses this to decide whether to log a "running MV3 SW as
    /// persistent background" caveat.
    pub fn is_service_worker(&self) -> bool {
        self.manifest_version == 3
    }
}

fn is_host_pattern(p: &str) -> bool {
    // Match patterns: `<scheme>://<host>/<path>`. Cheap heuristic — anything
    // with `://` we treat as a host pattern. Edge case (`http://`-only scheme
    // permission with no host) is rarely seen in real-world bundles.
    p.contains("://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mv2_manifest() {
        let json = r#"{
            "manifest_version": 2,
            "name": "Test",
            "version": "1.0",
            "background": { "scripts": ["a.js", "b.js"] },
            "permissions": ["cookies", "webRequest", "*://*.example.com/*"],
            "browser_specific_settings": { "gecko": { "id": "test@x.y" } }
        }"#;
        let m = ExtensionManifest::parse(json).unwrap();
        assert_eq!(m.manifest_version, 2);
        assert_eq!(m.background_scripts, vec!["a.js", "b.js"]);
        assert_eq!(m.permissions, vec!["cookies", "webRequest"]);
        assert_eq!(m.host_patterns, vec!["*://*.example.com/*"]);
        assert_eq!(m.extension_id, "test@x.y");
    }

    #[test]
    fn parses_mv3_manifest() {
        let json = r#"{
            "manifest_version": 3,
            "name": "Test",
            "version": "1.0",
            "background": { "service_worker": "background.js" },
            "permissions": ["cookies", "storage", "scripting"],
            "host_permissions": ["*://*.example.com/*"],
            "key": "ABC"
        }"#;
        let m = ExtensionManifest::parse(json).unwrap();
        assert_eq!(m.manifest_version, 3);
        assert_eq!(m.background_scripts, vec!["background.js"]);
        assert_eq!(m.permissions, vec!["cookies", "storage", "scripting"]);
        assert_eq!(m.host_patterns, vec!["*://*.example.com/*"]);
        assert_eq!(m.extension_id, "mv3@Test");
    }

    #[test]
    fn rejects_unsupported_version() {
        let json = r#"{"manifest_version": 4, "name": "x", "version": "1"}"#;
        assert!(matches!(
            ExtensionManifest::parse(json),
            Err(ManifestError::Unsupported(4))
        ));
    }
}
