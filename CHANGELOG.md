# Changelog

All notable changes are recorded here. Format roughly follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Entries
under *Unreleased* land in the next tag.

## [Unreleased]

### Added

- **Persistent `localStorage`.** `--storage-dir <DIR>` now keeps
  per-origin `localStorage` alive across runs, alongside the cookie
  jar that already shipped on this branch. Stored as
  `{storage_dir}/localstorage.json`, written atomically (tempfile +
  rename). Implementation:
  `obscura_net::LocalStorageStore` (per-origin
  `RwLock<HashMap<...>>`) wired into `ObscuraState` and reached from
  `bootstrap.js` via six new V8 ops
  (`op_localstorage_get_item` / `set_item` / `remove_item` / `clear`
  / `length` / `key`). The JS-side shim is a `Proxy` over a
  `Storage.prototype` core, so direct property access
  (`localStorage.foo`, `'foo' in localStorage`, `delete
  localStorage.foo`, `Object.keys(localStorage)`) round-trips
  through the ops as the spec expects. See
  [`docs/persistence.md`](docs/persistence.md).
- **`--storage-dir` on `obscura mcp`.** MCP sessions can now stay
  logged in across restarts. `BrowserState::new_with_storage` and
  `obscura_mcp::{run_with_storage, http::run_with_storage}` are the
  new entry points; the original `BrowserState::new` / `run`
  signatures are kept for back-compat. `browser_close` flushes
  the session before tearing the page down.
- **CLI `--eval` drains pending navigation.** When a `--eval`
  expression calls `form.submit()`, sets `location.href`, or
  otherwise triggers `op_navigate`, the CLI now drains the queued
  navigation (bounded to 10 redirect hops) before
  `save_session()` runs. Matches what CDP's `Runtime.evaluate`
  already does. Lets one-shot login flows work from the CLI without
  a CDP detour.
- **`BrowserContext::save_session()`.** Single graceful-exit entry
  point that flushes both `cookies.json` and `localstorage.json`.
- `LocalStorageStore` ships with 13 unit tests covering per-origin
  scoping, atomic save, lazy load, empty origins, and JSON shape.
- New integration test file
  `crates/obscura-cdp/tests/session_persistence.rs` (2 tests):
  - `session_persists_cookies_and_localstorage_across_contexts` —
    in-process HTTP server + two distinct `CdpContext` instances
    against the same `--storage-dir`, asserts cookies AND
    `localStorage` survive the cross-context boundary.
  - `localstorage_is_origin_scoped_in_memory_without_storage_dir` —
    same-origin navigation preserves `localStorage` even without
    `--storage-dir`. Confirms the Rust-side store works as the
    in-memory backing too.
- `docs/persistence.md` covering on-disk layout, write semantics,
  threat model, and the proxy/op architecture.

### Changed

- `bootstrap.js` `localStorage` is no longer a `{}`-backed closure
  that dies with the V8 isolate. It's now a `Proxy`-wrapped
  Storage-prototype object that dispatches to Rust-side ops, so the
  store survives navigation within a single run too (not just
  cross-run).
- README's "Browser extensions" table no longer flatly states that
  `localStorage` is in-memory per page — it now mentions that the
  user-visible `localStorage` is persisted under `--storage-dir`
  (the `chrome.storage.local` extension API is still in-memory).
- `obscura serve` / `fetch` `--storage-dir` flags now have
  doc-string help text. Previously the flag was undocumented in
  `--help`.

### Deprecated

- `BrowserContext::save_cookies()` is now a `#[deprecated]` alias
  for `save_session()`. The new name flushes both halves of the
  session; the old name only ever flushed cookies. External
  callers keep compiling.

### Fixed

- `crates/obscura-net/src/cookies.rs::test_save_load_roundtrip`
  was missing its `#[test]` attribute and never ran. Added the
  attribute; the dead-code warning is gone.
- Unused `use std::io::Write` in
  `cookies.rs::test_cookie_from_file_load_then_send_in_request`.

### Notes

- Plaintext on disk. Sites stash auth tokens in both cookies and
  `localStorage`; protect the `--storage-dir` directory accordingly
  (file permissions, encrypted volume).
- Single writer model. Two obscura processes pointed at the same
  `--storage-dir` will race. Same constraint as the existing
  cookie persistence.
- `sessionStorage`, IndexedDB, and `caches` API are deliberately
  NOT persisted (real browsers don't persist `sessionStorage`
  either; IndexedDB is a roadmap item).
- The save point is graceful-shutdown only for v1. A SIGKILL / panic
  before exit loses in-session mutations. A periodic flush and / or
  `Drop`-impl save is a worthwhile follow-up.

## [0.1.5] - earlier

Pre-changelog history. See `git log` for individual commits.
