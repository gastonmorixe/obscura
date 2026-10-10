# Private fixes audit (gm/private-upgrade-20261010)

Scope: symbol presence in the CURRENT worktree only. No build or nextest gate.
Status: Verified Offline.
Sources: `research/private-fixes-inventory.md`, `research/resolve-*.md`.

## Summary

All 8 checklist items: PASS.

| # | Item | Result |
| --- | --- | --- |
| 1 | WebExtension / obscura-ext + CLI `--extension` | PASS |
| 2 | localStorage ops + `--storage-dir` / LocalStorageStore | PASS |
| 3 | Stealth Chrome 148 wire identity + no cache-control/pragma | PASS |
| 4 | emulation-compression + gzip/brotli/deflate/zstd on wreq | PASS |
| 5 | document.referrer / extension header rewrite hooks | PASS |
| 6 | execute_script_guarded / watchdog on user-script paths | PASS |
| 7 | pub RpcResponse/RpcError, pub build_ax_nodes | PASS |
| 8 | dist CI for obscura-dist | PASS |

## 1. WebExtension / obscura-ext + CLI `--extension`: PASS

Evidence:

- Workspace member: `Cargo.toml:13` (`"crates/obscura-ext"`)
- Workspace dep: `Cargo.toml:32` (`obscura-ext = { path = "crates/obscura-ext" }`)
- Crate tree present: `crates/obscura-ext/` (`Cargo.toml`, `src/lib.rs`, `src/runtime.rs`, ...)
- CLI flag: `crates/obscura-cli/src/main.rs:55-64` (`#[arg(long, ...)] extension: Vec<PathBuf>`)
- Loader: `crates/obscura-cli/src/main.rs:353` `load_extension_runtime`, wired at `:451-452` and applied in `run_fetch` at `:1040-1041` via `context.with_extension`

## 2. localStorage ops + `--storage-dir` / LocalStorageStore: PASS

Evidence:

- Store type: `crates/obscura-net/src/localstorage.rs:33` `pub struct LocalStorageStore`
- Re-export: `crates/obscura-net/src/lib.rs:23`
- Six ops: `crates/obscura-js/src/ops.rs:5842` `op_localstorage_get_item`, `:5860` set, `:5875` remove, `:5890` clear, `:5905` length, `:5921` key
- Ops registered: `crates/obscura-js/src/ops.rs:7143-7148`
- Bootstrap Proxy: `crates/obscura-js/js/bootstrap.js:12201-12234` routes through `Deno.core.ops.op_localstorage_*`
- Runtime wiring: `crates/obscura-js/src/runtime.rs:1176` `set_localstorage_store`
- Context field: `crates/obscura-browser/src/context.rs:16` `localstorage_store: Arc<LocalStorageStore>`
- CLI flag: `crates/obscura-cli/src/main.rs:44-45` `#[arg(long)] storage_dir`

## 3. Stealth Chrome 148 wire identity + no cache-control/pragma: PASS

Evidence:

- UA: `crates/obscura-net/src/wreq_client.rs:61-62` `STEALTH_USER_AGENT` ... `Chrome/148.0.0.0`
- Client hints: `crates/obscura-net/src/wreq_client.rs:69` `STEALTH_SEC_CH_UA` ... `Chromium";v="148"`
- Applied on wire: `crates/obscura-net/src/wreq_client.rs:367-368` `.header("sec-ch-ua", ...)` / `.header("user-agent", ...)`
- Policy comments: `crates/obscura-net/src/wreq_client.rs:221` and `:363` (do NOT add cache-control/pragma)
- Grep for `.header("cache-control"` / `.header("pragma"` in `wreq_client.rs`: no matches

## 4. emulation-compression / gzip|brotli|deflate|zstd: PASS

Evidence:

- `crates/obscura-net/Cargo.toml:40` `wreq-util` ... `features = ["emulation-compression"]` (WSJ/DataDome comment at `:32-39`)
- Linux/Android wreq: `crates/obscura-net/Cargo.toml:65` features include `gzip`, `brotli`, `deflate`, `zstd` (plus `stream`)
- Non-Linux wreq: `crates/obscura-net/Cargo.toml:68` same compression set + `socks` + `stream`

## 5. document.referrer / extension header rewrite hooks: PASS

Evidence:

- Page hooks: `crates/obscura-browser/src/page.rs:1935` / `:1940` `collect_extension_request_headers`, `:2059` `apply_extension_request_headers`
- Navigate pre-pass: `crates/obscura-browser/src/page.rs:3661-3675` collects extras, mirrors Referer into `self.referrer`, applies headers
- Init: `crates/obscura-browser/src/page.rs:1784` `rt.set_referrer(&self.referrer)`
- Runtime: `crates/obscura-js/src/runtime.rs:1280` `pub fn set_referrer`
- DOM op: `crates/obscura-js/src/ops.rs:2189` `"document_referrer"` arm
- JS getter: `crates/obscura-js/js/bootstrap.js:5813` `get referrer() { return _domParse("document_referrer") ?? ""; }`

## 6. execute_script_guarded / watchdog on user-script paths: PASS

Evidence:

- Always-on guard: `crates/obscura-js/src/runtime.rs:3291-3302` `execute_script_guarded` always calls `execute_script_with_timeout(..., 5s)`
- Timeout path arms watchdog: `crates/obscura-js/src/runtime.rs:3315-3317` `arm_watchdog` / `disarm_watchdog`
- Page user-script call sites (sample): `page.rs:1897` ext preload, `:1969` prepass, `:2717` readyState, `:2731`/`:3645`/`:4926` preload, `:2842`/`:2858` classic scripts, `:3066` interactive, `:3152` DOMContentLoaded, `:3174` load, `:3805` iframe-load
- Extra exec_wd arming: `page.rs:2443`, `:3895`

## 7. pub RpcResponse/RpcError, pub build_ax_nodes: PASS

Evidence:

- `crates/obscura-mcp/src/lib.rs:38` `pub struct RpcResponse`
- `crates/obscura-mcp/src/lib.rs:48` `pub struct RpcError`
- `crates/obscura-cdp/src/domains/accessibility.rs:60` `pub fn build_ax_nodes(dom: &DomTree) -> Vec<Value>`

## 8. dist CI for obscura-dist: PASS

Evidence:

- Workflow present: `.github/workflows/dist-release.yml`
- Target repo: `:30` `DIST_REPO: <owner>/obscura-dist`
- Trigger: `:21-24` push to `gm/private` + `workflow_dispatch`
- Publish steps: `:155-176` softprops release to dist repo (`build-<epoch>` + rolling `latest`)

## Gaps / not claimed

- Compile, nextest, and obstacle-course gates were not run.
- Behavior of stealth against live WSJ/DataDome was not re-probed.
- Conflict-marker scan of the whole tree was not repeated here (resolve notes already report clean on the conflicted files).
