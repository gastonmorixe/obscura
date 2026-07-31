# Changelog

All notable changes are recorded here. Format roughly follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). Entries
under *Unreleased* land in the next tag.

## [Unreleased]

### Added

- **Persistent Fetch worker protocol.** `obscura-worker --fetch-protocol`
  provides a versioned, bounded NDJSON transport for lazy process reuse,
  ephemeral fetches, and up to eight named in-memory sessions. It enforces one
  live V8 isolate, saves named cookies and localStorage after each request and
  during eviction/shutdown, exposes status and graceful close operations, and
  retains the original no-argument scrape-worker protocol, and loads the first
  configured WebExtension for all worker-created contexts. See
  [`docs/persistent-fetch-worker-protocol.md`](docs/persistent-fetch-worker-protocol.md).
- **Accessibility Fetch output.** The persistent worker accepts
  `format: "accessibility"` and returns the CDP-compatible AXNode tree used by
  `Accessibility.getFullAXTree`. IDs derive from document-scoped DOM node IDs,
  and synthetic bounds are explicitly marked.
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

- **Dynamic classic scripts now load concurrently.** JavaScript-created
  classic `<script src>` elements no longer share the serialized ES-module
  import queue. This removes a large webpack chunk bottleneck observed on X:
  the old queue accumulated 14 to 32 scripts while the login application
  waited. Module imports remain serialized to protect `deno_core` from
  reentrant graph loading. Concurrent classic requests are now counted as
  pending work, the post-script settle loop will not report idle while either
  loader path is active, and blocked, non-success HTTP, fetch-failed, or
  evaluation-failed scripts dispatch `error` instead of incorrectly
  dispatching `load`. The dynamic-script CDP regression now verifies that a
  fast classic script can finish before an earlier slow one.
- **Text dumps ignore `<noscript>`.** `--dump text` previously reported inert
  no-JavaScript fallbacks as page content. On X this produced the misleading
  `JavaScript is not available` result even though the runtime, vendor, and
  main bundles had executed. Text extraction now skips `noscript` alongside
  `script` and `style`.
- Added a detailed account of the X login investigation, CLI wait and timeout
  semantics, the fixes above, remaining lifecycle and script-loader gaps, and
  recommended diagnostic commands in
  [`docs/X-login-JavaScript-investigation.md`](docs/X-login-JavaScript-investigation.md).
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

### Added

- **Extension request-header rewrites are honored on the document fetch.**
  Paywall-bypass extensions (e.g. Bypass Paywalls Clean) unlock article
  bodies by setting a `Referer` / `User-Agent` / `Cookie` on the top-level
  navigation, registered through `chrome.declarativeNetRequest`
  (`modifyHeaders` rules) or a blocking `webRequest.onBeforeSendHeaders`
  listener. Obscura used to drop both: `declarativeNetRequest.updateSessionRules`
  was a no-op and `webRequest` listeners were never invoked, so the
  rewritten header never reached the network and the page came back
  truncated. Now:
  - `chrome.declarativeNetRequest` maintains a real session/dynamic rule
    registry in the extension realm; the host harvests it after the
    background runs and parses `modifyHeaders` request-header rules
    (`obscura_ext::dnr`, with a faithful `urlFilter`/`regexFilter` matcher).
  - Blocking `webRequest.onBeforeSendHeaders` listeners run in a pre-pass
    against a synthetic `main_frame` request.
  - A navigation pre-pass (`Page::collect_extension_request_headers`) runs
    the extension once in a throwaway realm *before* the document fetch and
    applies the resolved rewrites to whichever HTTP client performs it
    (stealth wreq or plain reqwest).
  - New tests: 13 in `obscura-ext/src/dnr.rs`, header-resolution tests in
    `state.rs` / `runtime.rs`, and an end-to-end
    `obscura-browser/tests/extension_request_headers.rs` driving the real
    chrome-shim + V8 runtime.
- **`document.referrer`.** The `Document` object now exposes `referrer`
  (previously `undefined`). It's threaded from the navigation's referrer,
  including any an extension rewrote in. Sites and anti-bot/paywall logic
  commonly gate behaviour on `document.referrer`; exposing it makes
  headless renders match a real browser. (`obscura-js`, 2 tests)

### Fixed

- **WSJ (DataDome) now loads under `--stealth`.** WSJ articles were
  returning the DataDome CAPTCHA interstitial
  (`geo.captcha-delivery.com`) while Bloomberg (PerimeterX) worked.
  Root cause was the stealth client's HTTP request header *set*, not
  the TLS/HTTP2 fingerprint: captured against a live Chrome 148 on the
  same machine/IP, obscura's JA4
  (`t13d1514h2_8daaf6152771_9a55b862dad6`) and HTTP/2 Akamai digest
  (`52d84b11737d980aef856699f885ca86`) already matched byte-for-byte.
  The divergence was (a) **no `accept-encoding` header at all** — the
  wreq-util Chrome emulation only emits it under `emulation-compression`,
  which was off, and wreq carried no decode features, so advertising it
  manually would have broken response decompression; and (b) extra
  `cache-control: no-cache` + `pragma: no-cache` that a real Chrome
  address-bar navigation never sends. Both are header-set drift that
  DataDome scores. Fix:
  - `obscura-net/Cargo.toml`: enable `emulation-compression` on
    `wreq-util` (advertises `gzip, deflate, br, zstd` in Chrome's native
    header slot) paired with `gzip`/`brotli`/`deflate`/`zstd` on `wreq`
    (so the advertised encodings are actually auto-decompressed). The
    two move together by necessity.
  - `obscura-net/src/wreq_client.rs`: drop the manual
    `cache-control`/`pragma`; let the emulation own `accept-encoding`
    and the rest of the nav header set so wire order stays
    Chrome-correct. Only the version-identity overrides (sec-ch-ua 148
    brand list, UA string) and the hop-0 nav headers (`accept`,
    `upgrade-insecure-requests`, `priority`, `sec-fetch-user`) are still
    set by hand. Bloomberg/PerimeterX continues to pass.
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
