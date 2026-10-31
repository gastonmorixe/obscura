# Private fixes to preserve (origin/gm/private vs upstream/main)

Source tip: `5860cc9` on `origin/gm/private`.
Upgrade branch: `gm/private-upgrade-20261010` (merge in progress).

## Must-keep private commits

1. WebExtension host (`obscura-ext`) + DOM/runtime fixes
2. Persistent localStorage under `--storage-dir`
3. Stealth: Chrome 148 wire identity (Bloomberg/PerimeterX)
4. Stealth: accept-encoding + drop cache-control (WSJ/DataDome)
5. Stealth: Chrome 147+ nav headers completeness
6. Extension request-header rewrites + `document.referrer`
7. Deterministic multi-manifest selection
8. Persistent fetch worker, concurrent classic scripts, AX dumps
9. Always arm watchdog on user-script paths
10. Dist CI publish to private `obscura-dist` on `gm/private`
11. MCP `RpcResponse`/`RpcError` pub for private_interfaces

## Conflicted files (resolve keeping private + taking upstream structure)

- Cargo.toml / crate Cargo.toml: keep private crates/features (`obscura-ext`, stealth deps, localStorage)
- wreq_client.rs: keep private stealth identity/header fixes, adopt upstream API changes
- page.rs / bootstrap.js / ops.rs / runtime.rs: keep ext, localStorage, watchdog, referrer, fetch worker
- cli main.rs: keep storage-dir, extension, worker flags
- mcp: keep pub Rpc types if still needed
- accessibility + onload test: keep AX dump private bits if present
- Cargo.lock: regenerate after toml resolution via cargo
