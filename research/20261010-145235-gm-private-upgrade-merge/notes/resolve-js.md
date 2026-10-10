# Resolve: obscura-js merge conflicts

Branch: `gm/private-upgrade-20261010` (upstream/main into private).
Files: `bootstrap.js`, `ops.rs`, `runtime.rs`.

## Policy

Keep private: localStorage ops/persistence, extension-aware referrer docs, always-on watchdog for `execute_script_guarded`, concurrent classic script loading (via upstream's async classic path).
Adopt upstream: frames, import maps, write streams, document.domain, pending-script bridges, classic code cache, hierarchy-checked DOM mutations.

## bootstrap.js (6 hunks)

Took upstream for all hunks.

- Dynamic script queue: upstream `__runAsyncClassicScript` / parser-blocking / load-delaying / data-URL decode. Replaces private `__loadDynClassic` (same concurrent-classic intent).
- `appendChild`: upstream shadow detach + hierarchy check + `__prepareInsertedSubtree` (script prep lives there, not inline).
- `document.domain` getter/setter from upstream.
- localStorage Proxy and `document.referrer` bridge unchanged (outside conflicts).

## ops.rs (7 hunks)

1. Imports: upstream `obscura_net` block + `LocalStorageStore`.
2. `ObscuraState`: private extension-aware referrer docs + upstream `navigation_timing`.
3. `new()`: `navigation_timing: NavigationTiming::default()`.
4. DOM parse arms: upstream `document_origin` / `document_base_url` / `document_base_href`.
5. `query_selector`: upstream formatting.
6. Keep all six `op_localstorage_*` from private, then upstream frame message queue / `op_sleep(frame_id)` / frame realm / posted-task ops.
7. Extension: upstream `Cow::Owned(ops)` with localStorage ops pushed into the vec.

## runtime.rs (3 hunks)

1. `set_referrer` docs: keep private (extension rewrite note).
2. Take upstream `evaluate_prepared_module` + `execute_classic_script` + named `execute_script`. Override `execute_script_guarded` to always call `execute_script_with_timeout(..., 5s)` (private watchdog policy).
3. Pending-script status: upstream `__obscura_hasPendingDynamicScripts` plus load-delaying and parser-blocking helpers.

## Proof

```
rg '<<<<<<<|=======|>>>>>>>|\|{7}' \
  crates/obscura-js/js/bootstrap.js \
  crates/obscura-js/src/ops.rs \
  crates/obscura-js/src/runtime.rs
# exit 1, no matches
```

Status: Verified Offline (marker scan). No commit. No full cargo gate from this worker.
