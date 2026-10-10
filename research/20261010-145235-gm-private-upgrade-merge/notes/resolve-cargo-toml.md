# Cargo.toml merge resolution

Branch: `gm/private-upgrade-20261010` merging `upstream/main` into `origin/gm/private`.
Scope: root `Cargo.toml`, `crates/obscura-browser/Cargo.toml`, `crates/obscura-net/Cargo.toml` only.
`Cargo.lock` left untouched for a later regenerate pass.

## Cargo.toml (workspace root)

Conflict: workspace.dependencies entry after `obscura-mcp`.

| Side | Content |
| --- | --- |
| ours (HEAD / gm/private) | `obscura-ext = { path = "crates/obscura-ext" }` |
| theirs (upstream/main) | `obscura-render = { path = "crates/obscura-render" }` |
| ancestor | neither |

Resolution: keep both.

- Took theirs: `obscura-render` workspace path dep (upstream render crate wiring).
- Kept ours: `obscura-ext` workspace path dep (private WebExtension host).
- Order: `obscura-render` then `obscura-ext`, matching members list order nearby.
- Members already listed both crates outside the conflict hunk. No change there.

Why: dropping either breaks private extensions or the upstream render feature surface.

## crates/obscura-browser/Cargo.toml

Conflict: end of `[dependencies]` through `[dev-dependencies]`.

| Side | Content |
| --- | --- |
| ours | `[dev-dependencies]` with `tempdir = "0.3"` only |
| theirs | optional `image` dep + `[dev-dependencies]` with `tokio` test-util |
| ancestor | empty past `base64` |

Resolution: union.

- Took theirs: `image` optional dep (`jpeg`/`png`) required by `render = [..., "dep:image"]`.
- Took theirs: `tokio` workspace dev-dep with `test-util`.
- Kept ours: `tempdir = "0.3"` (used by `tests/extension_request_headers.rs`).
- Kept unconflicted ours: `obscura-ext = { workspace = true }`.

Why: render feature needs `image`. Private extension tests need `tempdir`. Upstream tests need tokio test-util.

## crates/obscura-net/Cargo.toml

Three conflict hunks.

### 1. `wreq-util` + `[dev-dependencies]`

| Side | Content |
| --- | --- |
| ours | long comment + `wreq-util` with `emulation-compression` |
| theirs | same `wreq-util` feature line + new `[dev-dependencies]` (`socket2`, `rcgen`, `tokio-rustls`) |
| ancestor | `wreq-util` without `emulation-compression` |

Resolution: ours comment + shared feature line + theirs dev-deps.

- Kept ours: WSJ/DataDome rationale comment for `emulation-compression`.
- Both sides already had `features = ["emulation-compression"]`. Kept that (private stealth fix also landed upstream).
- Took theirs: TLS fixture / socket2 `[dev-dependencies]` block.

### 2. Linux/Android `wreq` features

| Side | Features |
| --- | --- |
| ours | `prefix-symbols`, `socks`, `gzip`, `brotli`, `deflate`, `zstd` |
| theirs | same + `stream` |
| ancestor | `prefix-symbols`, `socks` only |

Resolution: took theirs (superset). Keeps private compression decode features and adds upstream `stream`.

### 3. Non-Linux `wreq` features

Same pattern as (2) without `prefix-symbols`.

Resolution: took theirs: `socks`, `stream`, `gzip`, `brotli`, `deflate`, `zstd`.

Why: private WSJ/DataDome path needs compression features + `emulation-compression`. Upstream needs `stream`. Superset drops nothing private.

## Proof

- Grep for conflict markers in the three files: no matches.
- Each file parses as TOML (`tomllib`): Verified Offline.
