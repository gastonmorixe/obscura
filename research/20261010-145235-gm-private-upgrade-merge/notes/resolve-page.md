# page.rs merge conflict resolutions

File: `crates/obscura-browser/src/page.rs`
Merge: `upstream/main` into private `gm/private` line.
Policy: keep private hooks, adopt upstream structure for new features.

## Hunk decisions

### 1. `Page` struct fields (title / referrer / viewport)

Decision: union.

- Keep `referrer` and extend its doc with private WebExtension Referer rewrite note.
- Take upstream fields: `viewport`, `screen_size_override`, `screen_metrics_emulated`, `locale_override`, `device_metrics_baseline`, `reduced_motion`, `device_scale_factor`, `default_background_color_override`.

Why: `init_js` already calls viewport / locale / screen APIs from upstream. Private only had referrer there.

### 2. `Page::new` field initializers

Decision: take upstream.

Initialize the new viewport / screen / locale / DSF / background fields. Keep `referrer: String::new()`.

### 3. `init_js` session history

Decision: take upstream.

After `rt.set_referrer`, call `predicted_session_history()` and `rt.set_session_history(...)`.

Private hooks already present outside the conflict: `set_localstorage_store`, extension preload via `execute_script_guarded`.

### 4. readyState `loading` script

Decision: keep private.

Use `execute_script_guarded` with upstream multi-line formatting.

### 5. Script loop after `ImportMap` (largest hunk)

Decision: take upstream structure.

HEAD still had the old `module_script` / combined load-events / settle-loop path. Upstream already replaced that with `ScriptKind::{Classic, Module}` plus `drive_load_delaying_scripts`.

Keep upstream `ScriptKind::Classic` (defer queue + `execute_classic` + document.write drive).

Private watchdog port after the structural take:

- `<ready-state-interactive>` -> `execute_script_guarded`
- `<dom-content-loaded>` -> `execute_script_guarded`
- `<load-event>` -> `execute_script_guarded`

Phase `exec_wd` arming was already outside the conflict and stays.

### 6. Navigate fetchStart / extension header pre-pass

Decision: union.

- Keep private `collect_extension_request_headers` / Referer mirror into `self.referrer` / `apply_extension_request_headers`.
- Add upstream `self.navigation_timing.record("fetchStart")` after that pre-pass.

### 7. Post-stylesheet install

Decision: take upstream.

`self.dom = Some(dom); self.init_js();` already runs before `fetch_stylesheets()`. HEAD's duplicate init plus `__obscura_css` inject is obsolete against native `author_stylesheets` install.

Keep upstream cleanup of `__obscura_registerLinkedStylesheet`, timeline origin reset, and render `reset_animation_timeline`.

Private `execute_script_guarded("<iframe-load>", ...)` after the hunk is unchanged.

## Private hooks verified present

- WebExtension preload + request-header rewrite path
- `document.referrer` threading + extension Referer mirror
- `localStorage` store wiring in `init_js` / extension prepass
- `execute_script_guarded` / `arm_watchdog` on user-script paths
- Upstream idle_deadline / PDF surfaces left intact outside conflicts

## Proof

- Grep for `<<<<<<<` in `page.rs`: no matches.
- No commit made.
