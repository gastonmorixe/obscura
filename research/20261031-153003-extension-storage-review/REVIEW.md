# Review: WebExtension host + Web Storage after upstream sync

Scope: merge commit `2fd8e6c` in worktree `/Users/gaston/Projects/obscura-worktrees/upstream-sync`
(branch `tmp/upstream-probe`). Read-only review. No source edits made by me.

Attribution baselines: `62aefe3` (fork, pre-sync) and `800b5ba` (upstream/main).

**Important note on the working tree.** During this review a peer process edited
`crates/obscura-browser/src/context.rs` and `crates/obscura-browser/src/accessibility.rs`
in place (uncommitted). All findings below are stated against **HEAD = `2fd8e6c`**, verified
with `git show HEAD:...`, not against the moving working tree. I flag this because the
working tree currently contains a legacy-migration function (`load_legacy_web_storage`)
and a DOM-id-derived AX `nodeId` that do **not** exist at HEAD. Do not read the working
tree as the merge result.

Evidence run in this session:

- `cargo check --release -p obscura-browser --features render --tests` -> `Finished`, no errors.
- `cargo check --release --workspace --features obscura-cli/render,obscura-cdp/render,obscura-mcp/render --tests` -> `Finished`, no errors (2 pre-existing deprecation warnings in `obscura-cdp`).
- `cargo nextest run --release --features render -p obscura-cdp --test integration -E 'test(session_persistence)'` -> **2 passed**, 147 skipped.
- `cargo nextest run --release --features render -p obscura-browser --test extension_request_headers` -> **4 passed**.

---

## WARNING

### W1. The merge dropped `set_localstorage_store` from the prepass and replaced it with nothing, so extension code in the prepass realm reads a throwaway store

`crates/obscura-browser/src/page.rs:2090-2123`

The prepass realm is built and wired by hand. Pre-merge it bound the fork's store:

```rust
// 62aefe3:crates/obscura-browser/src/page.rs
rt.set_cookie_jar(self.context.cookie_jar.clone());
rt.set_localstorage_store(self.context.localstorage_store.clone());   // <-- dropped
rt.set_http_client(self.http_client.clone());
```

At HEAD the line is simply gone, and no `set_local_storage` / `set_session_storage` /
`set_indexed_db` call replaced it:

```rust
// HEAD:crates/obscura-browser/src/page.rs:2111-2116
rt.set_cookie_jar(self.context.cookie_jar.clone());
rt.set_http_client(self.http_client.clone());
rt.set_dom(parse_html("<html><head></head><body></body></html>"));
rt.run_page_init();
```

Verified: grep of HEAD `page.rs:2090-2125` for any storage setter returns NONE.

Consequence. `ObscuraState::new` installs a private `Arc<OriginStorage>` by default
(`crates/obscura-js/src/ops.rs:652-653`, HEAD), so `localStorage` inside the prepass realm
does not throw. It silently reads and writes a fresh store that is dropped when the realm
is. The real page realm, immediately after, binds the context store correctly
(`page.rs:1982-1984`). So `localStorage` "works" in the prepass but shows different data
than the page will see, which is the worst failure mode: silent and plausible.

This is not a missing feature, it is a merge asymmetry. Every other store the prepass realm
needs is either bound (`cookie_jar`, `http_client`) or provided by the shim itself
(`chrome.storage._data`). The one that got dropped was dropped precisely because the type
it was bound to (`LocalStorageStore`) no longer exists, and no one re-pointed it at
`OriginStorage`.

Impact is narrow but real: an extension whose request-header rewrite is computed from
`localStorage` (a captured token, a per-site opt-in flag) computes it against empty storage
in the prepass, then registers that stale header for the document fetch. Header-path
extensions that only read `chrome.storage` are unaffected.

Fix: mirror the page-realm wiring at `page.rs:1982-1984`.

```rust
rt.set_cookie_jar(self.context.cookie_jar.clone());
rt.set_local_storage(self.context.local_storage.clone());
rt.set_indexed_db(self.context.indexed_db.clone());
rt.set_session_storage(Arc::new(OriginStorage::default())); // prepass has no real tab
rt.set_http_client(self.http_client.clone());
```

Note on `session_storage`: the prepass has no `Page`, so there is no tab-scoped store to
bind. A fresh default is correct here and is what the runtime already does implicitly;
passing it explicitly documents the decision. For `local_storage` and `indexed_db` the
context's `Arc` is the right source, matching the page path.

Test that would have caught this: in `crates/obscura-browser/tests/extension_request_headers.rs`,
seed `ctx.local_storage.set(origin, "k", "v")`, have `bg.js` read
`localStorage.getItem("k")` and register a header whose value is that read, then assert the
header equals `"v"`. Today that assertion fails (it gets an empty value), which is the
regression guard to add.

### W2. `--dump accessibility` passes an empty style map, so hidden nodes are reported as visible

`crates/obscura-cli/src/fetch_worker.rs:549-553`

```rust
fn dump_accessibility(page: &Page) -> String {
    let nodes = page
        .with_dom(|dom| obscura_browser::accessibility::build_ax_nodes_with_styles(dom, &Default::default()))
        .unwrap_or_default();
    json!({ "nodes": nodes }).to_string()
}
```

`build_ax_nodes_with_styles` skips a node when `context.is_hidden(id)`
(`crates/obscura-browser/src/accessibility.rs`), and `is_hidden` derives from the style map:
`style.display_none` and `style.visibility_hidden`
(`crates/obscura-browser/src/accessibility.rs:293,355-356`). With an empty map every lookup
falls back to `AccessibilityStyle::default()`, so `display:none` and `visibility:hidden`
subtrees are emitted as ordinary AX nodes.

The CDP path does it correctly and the asymmetry is the defect:

```rust
// crates/obscura-cdp/src/domains/accessibility.rs:20-23
let styles = page.js.as_ref().map(|js| js.accessibility_styles()).unwrap_or_default();
let nodes = page.with_dom(|dom| build_ax_nodes_with_styles(dom, &styles)).unwrap_or_default();
```

Attribution: this is **pre-existing, not merge-introduced.** The pre-merge fork's
`dump_accessibility` (`62aefe3:crates/obscura-cli/src/fetch_worker.rs:548-553`) called
`obscura_cdp::domains::accessibility::build_ax_nodes`, which was itself the
`&HashMap::new()` convenience wrapper (`62aefe3:crates/obscura-cdp/src/domains/accessibility.rs:59-62`).
The merge changed *which* zero-style helper it calls, not the behavior. So it is a real
bug, correctly worth fixing, but it did not regress here and should not block the sync.

Fix: mirror the CDP call, with the `render` gate the CDP path uses.

```rust
fn dump_accessibility(page: &Page) -> String {
    #[cfg(feature = "render")]
    let styles = page.js.as_ref().map(|js| js.accessibility_styles()).unwrap_or_default();
    #[cfg(not(feature = "render"))]
    let styles = std::collections::HashMap::new();
    let nodes = page
        .with_dom(|dom| obscura_browser::accessibility::build_ax_nodes_with_styles(dom, &styles))
        .unwrap_or_default();
    json!({ "nodes": nodes }).to_string()
}
```

---

## SUGGESTION

### S1. `session_persistence.rs` is sound but two assertions are looser than they read

`crates/obscura-cdp/tests/session_persistence.rs:167-213`

Checked specifically for vacuous passing, per the brief. The test is **not** vacuous:

- Line 174-178 asserts `!ls_files.is_empty()` before the entry loop, so a missing or empty
  `localStorage/` fails instead of skipping the loop.
- Line 197-200 uses `unwrap_or_else(|| panic!(...))`, so a file with no `entries` array
  fails instead of quietly contributing zero pairs.
- Lines 212-213 assert both `found_user` and `found_theme` after the loop.
- Run 2 (lines 216-265) builds a genuinely new `CdpContext` over the same `storage_dir` and
  reads back through a real navigation, so it proves reload rather than re-reading memory.

Two soft spots where a regression could still slip through:

1. Lines 190-191 say the origin "includes the random port" but never assert it. A change
   that wrote every origin into one file, or dropped the port from `storage_origin`,
   would still pass. Add `assert_eq!(ls_files.len(), 1)` (run 1 writes exactly one origin)
   and assert the parsed `origin` field starts with `http://127.0.0.1:`.
2. Lines 202-204 default a malformed pair to `""`/`""`. Harmless today because the match
   arms are exact, but a `.expect("entry must be a [key, value] pair")` would keep a format
   change from silently producing two empty strings.

### S2. `LocalStorageStore` is dead code at HEAD but still exported from `obscura-net`

`crates/obscura-net/src/lib.rs:23`

```rust
pub use localstorage::{origin_of, LocalStorageStore};
```

At HEAD nothing in the workspace imports either symbol (grep for `LocalStorageStore` and
`origin_of` across `crates --include=*.rs` returns only this re-export and
`localstorage.rs` itself). The module and its 13 unit tests now compile for nothing, and
its module doc still claims a `BrowserContext::save_session` hookup that the merge deleted.

The store was correctly dropped from `BrowserContext` (`context.rs:6` now imports
`OriginStorage`), so this is only leftover surface. Fix: delete
`crates/obscura-net/src/localstorage.rs` and the re-export in a follow-up commit. Note the
working tree is currently reusing this filename concept for a legacy migration, so land
that decision first.

### S3. Docs still describe the removed single-file layout, the old ops, and the removed API

- `AGENTS.md:88` lists `LocalStorageStore` as part of `obscura-net`.
- `README.md:503` shows `localstorage.json`.
- `docs/persistence.md:14,29,53,107-130,147,189` documents `localstorage.json`, the
  `op_localstorage_*` op names, and `save_session()`; line 189 describes a
  `#[deprecated]` alias that does not exist.

Actual code at HEAD: one file per origin under `{storage_dir}/localStorage/<stem>.json`
where the stem is a sanitized prefix plus an FNV-1a digest (`context.rs:59-76`), ops
`op_local_storage` / `op_session_storage` (`crates/obscura-js/src/ops.rs:6424,6446`), and
`save_storage()` / `save_local_storage()` (`context.rs:323-372`).

### S4. `chrome.storage` is per-realm, so extension storage does not survive a navigation

`crates/obscura-ext/js/chrome_shim.js:60-64,120-123`

`_storageAreas` is a closure local and each area's backing object is created fresh per
preload. The shim's own comment (lines 55-59) already says persistence "would belong on the
Rust side ... this lifts to `ExtensionState`". `ExtensionState` has the placeholders
(`crates/obscura-ext/src/state.rs:105-107`) and they are read by nothing.

Not a merge regression and not caused by the storage refactor: it was realm-local before
the sync and upstream's `OriginStorage` is not involved. Worth naming only because the doc
comment promises a lift that has not happened, and because with the dropped
`LocalStorageStore` there is no longer any candidate store to lift into.

### S5. `storage.onChanged` is a stub and `set` never fires it

`crates/obscura-ext/js/chrome_shim.js:96-100,509`

`set` mutates `_data` and calls back directly. `storage.onChanged` is
`{ addListener() {}, removeListener() {}, hasListener() { return false; } }`. An extension
that seeds a value and waits for its own `onChanged` will hang. Low priority, unchanged by
the merge. The `defaultSites` seed path (`runtime.rs:334-357`) writes `_data` directly and
does not need the event.

---

## Checks from the brief, answered with evidence

**1. Does the prepass still have a working document AND working `chrome.storage`?**

Document: **yes**. `collect_extension_request_headers` calls
`rt.set_dom(parse_html("<html><head></head><body></body></html>"))` at
`page.rs:2115` then `rt.run_page_init()` at `2116`, before `build_preload_script` at `2118`.
`run_page_init` (`crates/obscura-js/src/runtime.rs:2596-2603`, HEAD) evaluates
`globalThis.__obscura_init()`, so `globalThis.document` exists. The test
`prepass_initializes_document_before_content_script_registers_headers` passes and is a real
guard: its content script throws if `document.querySelector("body")` is null.

`chrome.storage`: **yes, and it is independent of the dropped store.** The shim builds
`_data` as `Object.create(null)` (`chrome_shim.js:64`) and exposes it as
`chrome.storage.local` (`chrome_shim.js:505`). It reads no `__obscuraCore`, no `op_*`, and
no storage op. `extension_request_headers` 4/4 pass, including the DNR and MV2 listener
paths.

But `localStorage` in the prepass realm is **unbound** (W1). That is the one storage
surface the merge broke, and it fails silently.

**2. Does the injected seed still target the shim's own backing store?**

**Yes.** `seed_js` (`crates/obscura-ext/src/runtime.rs:334-357`) writes
`chrome.storage.local._data.sites`, guarded by `if (_area && (!_have || ...))`. `_data` is
the shim's own object at `chrome_shim.js:64`, still exposed at `chrome_shim.js:505`. No
removed symbol is referenced. `inject_seed_after_import_scripts` (`runtime.rs:62-149`) is
byte-identical to pre-merge (`git diff 62aefe3..2fd8e6c -- crates/obscura-ext/` is empty),
and its 4 unit tests pass.

**3. Does `chrome_shim.js` reference any symbol that no longer exists?**

**No.** Every `__obscura_ext_*` global it reads is one it defines itself, or one that
`build_preload_script` sets before the shim body runs (`runtime.rs:393-400` sets
`_bundle`, `_manifest`, `_id`, `_url`, `_mv`). The only external dependency is
`globalThis.importScripts`, which the shim installs (`chrome_shim.js:49`). Nothing here
touches the storage refactor.

**4. `session_persistence.rs`: does it prove persistence, or can it pass vacuously?**

**It proves persistence.** Ported correctly to the per-origin layout. Details and two
tightening suggestions in S1. Both tests pass (verified, not assumed).

**5. `BrowserContext::isolated_copy`: does the extension survive, and does localStorage follow the split?**

Extension survives: `extension: self.extension.clone()` at `context.rs:314` (HEAD). Since
`ExtensionRuntime` holds `Arc<Bundle>` and `Arc<ExtensionState>`, clones share the DNR rule
registry, which is required: `collect_extension_request_headers` ingests harvested rules
into `ext.state` (`page.rs:2176`) and reads them back via `ext.resolved_request_headers`
(`page.rs:2179`). A per-copy `ExtensionRuntime` would break that.

localStorage split is correct: shared `Arc` for persistent, fresh `OriginStorage::default()`
for incognito (`context.rs:294-298`), matching `indexed_db` on the next line (`299-300`).
`storage_dir` is `None` for incognito via `persistent.then(...)` (`context.rs:312`), so an
incognito copy cannot write the profile. Covered by
`local_storage_stays_in_memory_without_a_storage_dir` (`context.rs:585-606`).

Two consequences worth a deliberate decision, not a change:

- `isolated_copy(persistent=true)` is what the CDP server uses per connection
  (`crates/obscura-cdp/src/server.rs:837`) and for its persistence template (`539`).
  Because the store is shared, two connections writing one origin race on the same
  `OriginStorageInner`. `flush_changed` holds the mutex across the write callback
  (`crates/obscura-js/src/ops.rs:300-306`), so they serialize and no torn file is possible.
  Intended, and documented at `context.rs:291-293`.
- `create_browser_context` (`crates/obscura-cdp/src/dispatch.rs:285`) passes
  `persistent=false`, so `Target.createBrowserContext` gets an isolated incognito store and
  `storage_dir = None`. It also inherits `extension`, so an attached extension runs there
  too. Both match Chrome.

**Out of scope, noted while reading.** Neither `crates/obscura-cdp/src/server.rs` nor
`crates/obscura-mcp/src/lib.rs` mentions `with_extension` at HEAD, so `obscura serve` and
`obscura mcp` never attach an extension. Pre-merge `server.rs` had 0 `extension` references,
so this is pre-existing, not a merge regression. Also, `fetch_worker.rs:44` reads
`OBSCURA_FETCH_WORKER_EXTENSION`, and no code in `crates/obscura-cli/src/` sets it, so that
env path is currently unreachable dead input.

---

## Verdict

The merge is sound on the question the brief actually asks. The extension host does not
depend on the dropped `LocalStorageStore`: the prepass realm builds its own document via
`set_dom` + `run_page_init` before the preload runs, `chrome.storage` is the shim's own
realm-local `_data` and touches no storage op, and the `defaultSites` seed injected after
literal `importScripts(...)` still writes that same `_data`, with the string rewriter byte
identical to pre-merge. `chrome_shim.js` references no removed symbol. `isolated_copy`
carries the extension through a shared-context clone (correct and load-bearing, since DNR
state must be shared) and splits localStorage properly: shared `Arc` for persistent, fresh
store plus `storage_dir = None` for incognito. Both target suites pass (2/2 and 4/4) and the
workspace builds clean. One merge-introduced defect should be fixed before shipping: the
prepass realm lost its `set_localstorage_store` and nothing replaced it, so
`localStorage` there silently resolves to a throwaway `OriginStorage` and an extension
computing a header from `localStorage` gets empty data (W1). Everything else is either
pre-existing (`--dump accessibility` ignoring computed styles, W2), dead code
(`LocalStorageStore`), doc drift (`docs/persistence.md`, AGENTS.md, README), or unchanged
behavior (`chrome.storage` not surviving navigation). Separately, be aware the working tree
is being edited by another process and now contains a legacy `localstorage.json` migration
and a different AX id scheme that are absent from HEAD; review those as new work, not as
the merge result.
