# Resolve: CLI / MCP / CDP conflict files

Merge: `upstream/main` into private line on `gm/private-upgrade-20261010`.
Scope: five allowlisted files only. No commit.

## Decisions

### `crates/obscura-cli/src/main.rs`

- Kept private `load_extension_runtime` and `--extension` wiring into `run_fetch`.
- Adopted upstream stack-boosted `main` + `operator_network_error` + `run_cli`.
- `run_fetch` takes both private `extension` and upstream `obey_robots` / `screenshot` / `wait_is_fixed`.
- MCP arm keeps private `storage_dir` field and calls `run_with_storage`.
- Bare `--eval` path uses private `context.save_session()` (cookies + localStorage).
- Dump-text filter keeps private `noscript` strip plus upstream formatting.

### `crates/obscura-mcp/src/lib.rs`

- `RpcResponse` / `RpcError` remain `pub` for private_interfaces.
- Kept explicit `new_with_storage(..., storage_dir)`.
- `new()` also reads `OBSCURA_STORAGE_DIR` (upstream env path) then delegates.
- Stdio `run_with_storage` uses storage constructor plus upstream `operator_network_hints` and runtime pump.

### `crates/obscura-mcp/src/http.rs`

- Uses `BrowserState::new_with_storage` with the CLI-passed `storage_dir`.
- Keeps upstream Arc origins/auth, connection semaphore, and request channel.

### `crates/obscura-cdp/src/domains/accessibility.rs`

- `build_ax_nodes` stays `pub` (fetch-worker AX dump).
- Body is upstream `build_ax_nodes_with_styles` + hidden-node filter.
- AX ids stay private DOM-scoped `ax-{domId}` (tests + dump stability).
- Dropped dead local `child_ids` walk; parentId pass still fills `childIds`.

### `crates/obscura-cdp/tests/dynamic_script_onload_fires.rs`

- Upstream two-phase: at `waitUntil=load` all false (plus empty `loadOrder`).
- After explicit `settle`, all true with private `loadOrder:["nested","direct"]`.

## Proof

Search for `<<<<<<<` in the five files: zero matches.

## Status

Implemented. Conflict markers removed. Compile/test gate not run here (leaf worker scope).
