# Browser extensions in Obscura

This document is the deep dive on how Obscura runs WebExtensions: what
works, what doesn't, and the reasoning behind each architectural
choice. The README has a quick start — come here when you're
integrating an extension or debugging why a content-script-driven
extension stopped working after a site change.

Obscura's V8 already runs page JavaScript, and per-page DOM mutations
already route through `op_dom`. Running a WebExtension on top of that
foundation is mostly bookkeeping: which scripts run on which URLs,
where the registered listeners live, how `chrome.storage.local` is
backed. The architecture below is the result of optimising for one
constraint above everything else — **the extension must finish its
work synchronously, inside a single preload script, before Obscura's
text/HTML extractor walks the DOM**. That single constraint
determines every other design decision.

## Architecture overview

```
                  Obscura                                          ┌──────────────┐
┌────────────────────────────────┐                                 │ bundle on    │
│ obscura-cli                    │     --extension <path>          │ disk         │
│  - parse Args                  │ ──────────────────────────────► │ (.xpi/.zip/  │
│  - load Bundle once at startup │                                 │  .crx/dir)   │
└──────────────┬─────────────────┘                                 └──────────────┘
               │ Arc<ExtensionRuntime>
               ▼
┌────────────────────────────────┐
│ BrowserContext                 │
│  - cookies, http client, …     │
│  - extension: Option<Arc<…>>   │
└──────────────┬─────────────────┘
               │ shared across every Page in this context
               ▼
┌────────────────────────────────┐     fetch HTML, parse to DomTree
│ Page::navigate(url)            │ ───────────────────────────────────► obscura-dom
│  1. init_js() (fresh V8)       │
│  2. *** inject preload ***     │ ◄────────────────────────────────┐
│  3. (page scripts skipped at   │                                  │
│     domcontentloaded)          │                                  │
└──────────────┬─────────────────┘                                  │
               │                                                    │
               ▼                                                    │
        ┌──────────────┐                                            │
        │ V8 realm     │                                            │
        │  ┌────────┐  │                                            │
        │  │ chrome │◄─┼── injected via execute_preload_script  ◄───┘
        │  │ shim   │  │   (one big concatenated JS blob built by
        │  ├────────┤  │    ExtensionRuntime::build_preload_script)
        │  │ bg js  │  │
        │  └────────┘  │
        │      │       │
        │      │ shim fires synthetic tabs.onUpdated
        │      ▼       │
        │  listener queues setTimeout()s for runOnTab work
        │              │
        │ tabs.executeScript(file) → file loaded synchronously into same realm
        │              │
        │ tabs.sendMessage(msg) → runtime.onMessage listeners called synchronously
        │              │
        │              ▼
        │       DOM mutated (<p>s injected, classes removed, etc.)
        └──────────────┘
               │
               ▼
   --dump text / --dump html walks the post-mutation DOM
```

Everything happens in **one V8 realm**, on **one synchronous code
path**. No second isolate for the "background world". No async drift.
When `init_js()` returns, the extension's work is complete and the
DOM is ready for extraction.

## Crates and modules

### `crates/obscura-ext/`

The whole extension subsystem lives here. ~1200 LOC of Rust and
~600 LOC of JavaScript (the `chrome` shim).

| File | Role |
|---|---|
| `src/lib.rs` | Public surface; re-exports `Bundle`, `ExtensionRuntime`, `ExtensionState`, `ExtensionManifest`. |
| `src/manifest.rs` | Parses `manifest.json`. Handles both MV2 (`background.scripts`, host patterns mixed into `permissions`) and MV3 (`background.service_worker`, `host_permissions`). Unsupported `manifest_version` values error explicitly. |
| `src/bundle.rs` | Reads a bundle from disk in 4 shapes: unpacked directory, `.zip`, `.xpi` (zip), `.crx` (zip with a CRX2 or CRX3 header that we strip). Auto-detects from filesystem. All files kept in memory keyed by forward-slash relative path. |
| `src/host_match.rs` | WebExtensions match-pattern matcher (`*://*.example.com/*`). Cheap enough to test against many URLs. |
| `src/state.rs` | Mutable state shared between the host and JS: storage areas (`local` / `sync` / `session` / `managed`), registered `webRequest` listeners, declarativeNetRequest blocks. |
| `src/runtime.rs` | The integrator. `ExtensionRuntime::build_preload_script(url)` produces the per-page JS blob that wraps the `chrome` shim + background scripts + onUpdated trigger inside an IIFE. |
| `js/chrome_shim.js` | The `chrome` / `browser` global. ~470 lines covering ~50 API methods, most no-ops; the ones with real semantics are `runtime.{getManifest,sendMessage,onMessage}`, `storage.local.{get,set,remove,clear}`, `tabs.{query,sendMessage,executeScript,onUpdated}`, `scripting.executeScript`, `permissions.contains` (auto-grant). |

### Glue in other crates

| Crate | Change |
|---|---|
| `obscura-browser` | `BrowserContext` gained `extension: Option<Arc<ExtensionRuntime>>`. `Page::init_js` calls `ext.matches_url(url)` and, if true, runs the preload script via `execute_preload_script`. |
| `obscura-cli` | New global `--extension <PATH>` flag. `load_extension_runtime` is called once at startup; the resulting `Arc<ExtensionRuntime>` is threaded into every `BrowserContext`. |
| `obscura-js` | Two new ops: `query_selector_within` and `query_selector_all_within`. Element and DocumentFragment `querySelector(All)` on the JS side route to these instead of the document-scoped variants. `bootstrap.js` gained a set of DOM-class stubs (`NamedNodeMap`, `Attr`, …) and a real `DOMParser`. The text extractor preserves inline whitespace. |
| `obscura-dom` | `DomTree::query_selector_within(root, sel)` / `query_selector_all_within(root, sel)`. Old `query_selector` / `query_selector_all` now delegate to these with `root = document()`. |

## The preload script, step by step

When `Page::navigate(url)` is called and the context has an extension
attached:

### 1. URL gate

```rust
if !ext.matches_url(url) { return; }
```

The manifest's `host_permissions` (or MV2 `permissions`) are compiled
once into `MatchPattern` structs and tested against the target URL.
Misses skip the preload entirely — no V8 work, no parsing.

### 2. Build the preload string

`build_preload_script(url)` produces a single JS string with this
structure:

```js
(function obscuraExtensionBootstrap() {
  if (globalThis.__obscura_ext_loaded) return;     // idempotency guard
  globalThis.__obscura_ext_loaded = true;
  globalThis.__obscura_ext_bundle = { ... };        // every .js in the bundle
  globalThis.__obscura_ext_manifest = { ... };      // synthesised manifest subset
  globalThis.__obscura_ext_id = "...";              // extension id
  globalThis.__obscura_ext_url = "...";             // current page URL
  globalThis.__obscura_ext_mv = 2;                  // manifest_version

  // === chrome / browser shim ===
  // (the entire contents of chrome_shim.js, ~470 lines)
  // ...defines globalThis.chrome, globalThis.browser

  // === bundle's background scripts in load order ===

  // === (optional) storage seed injected before the last bg script ===
  // For extensions that gate behaviour on a "site enabled" map in
  // storage.local, we pre-populate that map from the bundle's own
  // default-sites tables so the extension behaves as if every site
  // it ships rules for is enabled. See ExtensionRuntime in runtime.rs
  // for the exact heuristic.

  // === trigger ===
  globalThis.__obscura_ext_fire_loaded(url);
  globalThis.__obscura_ext_drain_timers(50);
})();
```

Eval cost in V8 with a fresh isolate runs ~30–50 ms for a ~1 MB
extension bundle.

### 3. The synthesised tabs.onUpdated event

Real Chrome fires `tabs.onUpdated` events with `{status: 'loading'}`
and later `{status: 'complete'}` as the page loads. Obscura's
one-page-per-context model has nothing to listen on; we synthesise a
single event with `{status: 'complete'}` for the current page right
after the background scripts finish evaluating.

This is the standard hook point for extensions: a typical MV2
background script does

```js
chrome.tabs.onUpdated.addListener((tabId, changeInfo, tab) => {
  if (/^http/.test(tab.url) && changeInfo.status === 'complete') {
    setTimeout(() => {
      if (siteEnabled(tab)) runOnTab(tab);
    }, 0);
  }
});
```

`runOnTab` (or whatever the extension calls it) chains
`tabs.executeScript({file: '...'}, callback)` for each content script
it wants to inject, then dispatches a message to the content scripts
with whatever per-site config it computed.

### 4. Synchronous setTimeout drain

This is the single most important architectural choice. Real-browser
`setTimeout(fn, 0)` defers to the next event-loop tick. Obscura's V8
runs the preload as one big synchronous evaluation; by the time we
return to Rust, no microtask has fired, and "fired in a microtask" is
indistinguishable from "fired after `--dump text` already ran".

The `chrome` shim therefore overrides `globalThis.setTimeout` (and
`setInterval` / `clearTimeout`) with a synchronous queue plus a drain
function. The flow:

1. The preload IIFE fires the onUpdated event. The bg listener runs
   and queues a `setTimeout(fn, 0)` — appended to `_timer_queue`.
2. Preload calls `__obscura_ext_drain_timers(50)`.
3. The drain pops timers FIFO and invokes them. Each callback can
   push new timers onto the queue (it's common for extensions to
   queue ~5 retries spaced 200 ms apart, plus a follow-up
   `sendMessage` call). The loop continues until either the queue is
   empty or we hit `50` iterations (sanity cap).
4. When the drain returns, every extension code path has run to
   completion. The DOM has been mutated. Extension JS is done.

`maxIters = 50` is a safety net against infinite re-queueing (a buggy
extension could `setTimeout(itself, 0)` forever). A well-behaved
extension drains in 15–25 iterations.

### 5. tabs.executeScript dispatches synchronously

The same reasoning applies to `tabs.executeScript`'s callback. Real
Chrome posts the result to a different process and the callback fires
on the next tick. We could do that — but a 3-deep nested callback
chain (load `lib.js` → load `cs.js` → load `cs_per_locale.js`) would
then complete only after the drain finished, by which point the
bg-to-cs message had already fired against a missing listener.

So `_tabsExecuteScript` and `_scriptingExecuteScript` invoke the
callback synchronously after `(0, eval)(src)`. The content scripts
register their `runtime.onMessage` listener before the message
arrives. Order: deterministic, correct, identical-result-every-run.

### 6. tabs.sendMessage / runtime.sendMessage dispatch in-realm

Both invoke `_dispatchMessage(msg, sender)`, which iterates
`_onMessageListeners` and calls each one synchronously. No IPC, no
second process, no microtask boundary — the listener function is
called right there. By the time the sender's `.catch(…)` is attached
to the returned promise, the listener has finished.

This is what makes background-to-content-script messaging actually
work in Obscura's model: same realm, same call stack.

### 7. Idempotency guard

The IIFE bails out with `if (globalThis.__obscura_ext_loaded) return;`
at the top. `Page::init_js` rebuilds the V8 isolate on every
navigation, so under normal use this guard never trips — but it's
there because a CDP client might call
`Page.addScriptToEvaluateOnNewDocument` and then renavigate without
realising init_js already wired the extension in.

## Bootstrap.js changes (and why each is necessary)

`obscura-js/js/bootstrap.js` is the script V8 evaluates inside its
startup snapshot. Five changes landed to bring extension support up
to "runs real-world MV2 extensions cleanly":

### `HTMLScriptElement.text` alias

Real Chrome exposes `.text` as a getter/setter on script elements
that aliases `textContent`. Many content scripts (and libraries like
`json5`, `DOMPurify`, etc.) read inline JSON payloads via
`scriptElement.text`. Without the alias, `JSON.parse(undefined)`
throws `SyntaxError: "undefined" is not valid JSON` and the extension
aborts.

### Element-scoped querySelector

This was a real Obscura DOM bug — not just an extension issue.
`Element.prototype.querySelector(sel)` was wired to a document-rooted
op, so calling `someDiv.querySelector('p')` returned the first `<p>`
*anywhere in the page*, not the first `<p>` under `someDiv`. The bug
went unnoticed because most production code paths happened to want
the document scope. Extensions expose it because a common idiom is

```js
const wrapper = document.createElement('div');
wrapper.innerHTML = '<textarea>' + s + '</textarea>';
return wrapper.querySelector('textarea').value;
```

The textarea exists under the wrapper but is detached from the live
document, so the document-scoped query returned null and `.value`
threw.

We added `DomTree::query_selector_within(root, sel)` /
`query_selector_all_within` in `obscura-dom`, exposed them as ops,
and re-pointed `Element.prototype.querySelector` and the
`DocumentFragment` equivalent at the new ops.
`Document.prototype.querySelector` stays on the old document-scoped
variant (correct behaviour there). CDP `DOM.querySelector` is
unchanged.

### Real `DOMParser`

The old `DOMParser` stub returned `globalThis.document` for every
`parseFromString(...)` call. That's worse than null because callers
happily do `doc.querySelector(...)` and get a live page element, then
`elem.appendChild(that_element)` ripping it out of the page DOM.

The new `DOMParser` creates a detached `<div>`, sets `innerHTML` to
the parsed string, and returns a small object with the methods the
spec requires (`querySelector`, `body`, `documentElement`,
`textContent`, etc.) backed by the temp wrapper.

### Missing DOM constructor classes

HTML-sanitiser libraries do `x instanceof Foo` to feature-detect. If
`Foo` is undefined V8 throws "Right-hand side of 'instanceof' is not
an object" and tears down whatever call chain it's in. We added empty
constructor classes for: `NamedNodeMap`, `Attr`, `ShadowRoot`,
`NodeIterator`, `TreeWalker`, `CDATASection`,
`ProcessingInstruction`, `HTMLCollection`, `NodeList`, plus aliases
`HTMLFormElement`, `HTMLInputElement`, `HTMLAnchorElement`,
`HTMLIFrameElement`, `HTMLImageElement`, `HTMLDocument`,
`SVGElement`.

They're constructable but empty. `instanceof` returns false (which is
correct — our actual elements are `Element`, not the spec-named
subclasses). Callers accept that, fall through to the next branch of
their detection, and work.

### Inline-whitespace preservation in `--dump text`

The old `extract_readable_text` ran `contents.trim()` on every text
node, which collapses leading/trailing whitespace carrying the
inter-token space between inline children. "Judge X said" with X as
a link became "JudgeXsaid". The new logic collapses runs of
whitespace to a single space but preserves leading/trailing spaces if
the source had them.

This change affects all `--dump text` consumers, not just extensions
— a small fidelity improvement that happened to be on the critical
path for getting extension-injected paragraphs to read like real
article text.

## Storage semantics

`chrome.storage.local.get` and `set` are in-memory per page realm.
They're not persisted across navigations and not synced to disk.

Why this is fine for most cases: extensions tend to write to storage
when the user toggles a per-site setting in the extension's options
page. Obscura doesn't render that UI; storage is preseeded at boot
when the bundle ships a default-sites table that the runtime can
introspect. Everything the extension ever reads from storage was put
there during the same preload that's about to read it.

When persistence matters (multi-navigation scrape sessions, `obscura
serve` with multiple CDP clients), the path forward is:

1. Hoist `_storageAreas` from `chrome_shim.js` to `ExtensionState`
   (Rust-side; the `StorageArea` placeholder is already there).
2. Add `op_extension_storage_get` / `_set` / `_remove` / `_clear`.
3. Plumb the area name through.
4. Optionally: write to `~/.cache/obscura/<context_id>/storage.json`
   on every write (debounced) for cross-session persistence.

## webRequest, declarativeNetRequest

Listeners register without throwing — `addListener`,
`removeListener`, `hasListener`, `hasListeners`, the
`OnBeforeRequestOptions` / `OnBeforeSendHeadersOptions` /
`OnHeadersReceivedOptions` constants including `EXTRA_HEADERS` — but
**no synchronous interception** runs against real network traffic
yet.

`obscura-net::interceptor::RequestInterceptor` exists in the codebase
and CDP's Fetch domain already implements pause/resume. The remaining
work is to install a `RequestInterceptor` impl on
`ObscuraHttpClient` that walks `ExtensionState.listeners_for(event)`,
evaluates each listener's callback in V8 (with the URL details), and
maps the return value (`{cancel:true}` / `{redirectUrl:'...'}` /
`{requestHeaders:[…]}` / `{responseHeaders:[…]}`) onto
`InterceptAction`.

That's a sync-vs-async boundary worth thinking about carefully — the
HTTP client is async, V8 isn't. The cleanest approach is probably:
extract the listener body once at registration time, walk its AST
for the static cases (URL filter, cancel/redirect verdict), compile
to a plain Rust rule. Listeners with dynamic logic in their callback
body fall through to a slower "call V8 from the interceptor" path
(blocking on a oneshot channel into the JS thread). Or just don't
support them and document it.

For extensions whose entire bypass is content-script-driven (DOM
mutation only) this layer is unnecessary. Extensions that depend on
blocking the network for a specific URL pattern at request time
currently work less well than they would with the full webRequest
path.

`declarativeNetRequest.updateSessionRules` accepts rules but does
nothing with them. Same plan as webRequest: install the rules in a
Rust-side rule engine consulted by the http client. The complication
is that DNR has its own URL-filter grammar (`||`, `^`, regex
fragments, priority arbitration) that we'd need to either
re-implement or delegate to a library.

## What runs where, in one paragraph

When Obscura fetches a URL with `--extension`: the HTML is fetched
and parsed (Rust) → a V8 isolate is created (`init_js`) → the
extension's preload runs (chrome shim + background scripts +
onUpdated + drain of all queued work) → the extension mutates the
DOM via `op_dom` → page JavaScript optionally runs
(`execute_scripts`, skipped under `--wait-until domcontentloaded`) →
`--dump text/html/markdown` walks the final DOM. Everything is
single-threaded and synchronous within a navigation; there's no race
window between extension bringup and DOM extraction.

## CLI surface

The flag is global. It works with every subcommand that builds a
`BrowserContext`:

```bash
obscura --extension <path> fetch <url>             # one-shot
obscura --extension <path> serve --port 9222       # CDP (per-target)
obscura --extension <path> scrape <urls…>          # parallel workers each load the bundle
obscura --extension <path> mcp                      # MCP server's BrowserContext
```

`<path>` is auto-detected:

- Filesystem directory containing `manifest.json` → unpacked
  extension.
- `*.zip` → standard zip with `manifest.json` at the root (or one
  level deep — a single wrapper directory is stripped).
- `*.xpi` → same as `.zip`. Firefox's signed releases.
- `*.crx` → Chrome's CRX2 (16-byte header + pubkey + signature +
  zip) or CRX3 (12-byte header + protobuf header + zip). The header
  is stripped automatically.

Pass it once. Passing `--extension` more than once warns and uses
the first; multi-extension is roadmap.

## Diagnostics

Bring-up emits one INFO line through the `obscura::console` target.
Enable info-level for that target to see it:

```bash
RUST_LOG=obscura::console=info,obscura_ext=info \
  obscura --extension ./ext fetch <url> --stealth --dump text 2>&1 \
  | grep -E 'obscura-ext|console'
```

A successful run looks like:

```
INFO obscura_browser::page: obscura-ext: injecting preload (… bytes) for url=https://…
INFO obscura::console: obscura-ext: preload starting (N bundled scripts)
INFO obscura::console: obscura-ext: drained K timers, onUpdated listeners=M
```

Failures emit ERROR-level messages from the same target:

```
ERROR obscura::console: obscura-ext: chrome shim failed: <err>
ERROR obscura::console: obscura-ext: background scripts failed: <err>
ERROR obscura::console: obscura-ext: timer threw: <err>
ERROR obscura::console: obscura-ext: executeScript failed for <file>: <err>
ERROR obscura::console: obscura-ext: onUpdated listener threw: <err>
```

For deeper inspection — what JS each setTimeout was queued from,
body of each drained timer — set
`globalThis.__obscura_ext_trace_timers = true` at the top of the
IIFE (`crates/obscura-ext/src/runtime.rs`
`build_preload_script`). The flag is opt-in to avoid drowning normal
runs in trace output.

## Adding a new extension

The shim is shaped by what real-world MV2 extensions touch on
bringup. Loading a different extension is mostly about (a)
confirming the manifest loads cleanly and (b) running it with
verbose logging until the error messages narrow down which API
surface needs a real implementation versus a no-op.

A reasonable workflow:

1. `obscura --extension <new-bundle> fetch <its-target-url> --dump text`
2. Watch `RUST_LOG=obscura::console=info` output. Any ERROR from
   `obscura-ext` is a missing API the extension touched at startup —
   add it to `chrome_shim.js` as a no-op (or, if the extension's
   logic depends on the return value, a real impl).
3. If the bringup runs but the DOM mutation doesn't land, enable
   `__obscura_ext_trace_timers` and watch which `executeScript` /
   `sendMessage` / `setTimeout` calls fire.
4. If the extension relies on `webRequest` blocking, you'll need
   the interceptor wiring (see "webRequest, declarativeNetRequest"
   above) — currently a manual project, not a hot-swap.

The DOM is the source of truth. Everything an extension does — DOM
mutation, content-script eval, message dispatch, storage —
observably either lands in the page DOM by the time the preload
returns, or doesn't. Tracing is the debugging tool.

## What's not supported (yet)

- **`chrome.webRequest.*` interception** — listeners register but
  do not intercept real traffic. Sites that need network-side
  blocking fall back to whatever the content script can do
  post-load.
- **`chrome.declarativeNetRequest`** — rules accepted, not
  enforced.
- **Storage persistence across navigations** —
  `chrome.storage.local` is in-memory per page realm.
- **The popup/options UI** — never rendered. Settings that depend
  on user interaction need to be set programmatically (edit the
  bundle's default-sites table ahead of time, or pre-seed storage
  at startup).
- **MV3 service-worker lifetime** — MV3 extensions load and their
  `service_worker` runs as a persistent script in the page realm,
  not as a real SW. There's no idle suspension, no
  `chrome.alarms`-driven resurrection, no `runtime.onSuspend`. This
  is fine for extensions whose SW keepalives itself, but extensions
  that rely on SW lifecycle events as state-machine ticks will
  miss them.
- **Multi-extension** — exactly one `--extension` is honoured.
  Passing more emits a warning.
- **CSP for extension pages** — manifest's
  `content_security_policy.extension_pages` is ignored. We don't
  render extension HTML pages.
- **`chrome.cookies` real backing** — currently returns empty for
  `get` and accepts `set`/`remove` as no-ops. The underlying
  `CookieJar` exists in `obscura-net` and is wired to navigation;
  integrating it with `chrome.cookies.*` is straightforward but
  not done.

## Performance

For a single fetch with `--extension <bundle>`:

- Bundle load (cold, from `.xpi` or `.crx`): tens of ms (zip
  decompression).
- Manifest parse + match pattern compile: a few ms (linear in the
  number of host patterns).
- Preload script construction: tens of ms (serialising the bundle
  contents into the format string).
- V8 eval of preload: tens of ms (the bundle is the bulk; chrome
  shim is ~5 ms on its own).
- Per-navigation extension overhead after the cold load: typically
  ~85–150 ms.

Cold load happens once at CLI startup; the bundle is reused across
all pages in the same process. For `obscura scrape` with N workers,
each worker reloads the bundle (separate processes) — that's a
fixed cost per worker, not per URL.

Memory: a few MB to ~10 MB of resident JS per V8 isolate. Each
`Page` creates and drops an isolate per navigation, so the
extension memory is reclaimed between fetches.

## Where to look in the source

If you're debugging or extending:

- **The chrome.* surface** — `crates/obscura-ext/js/chrome_shim.js`.
  Search for the API name you care about.
- **Preload assembly** —
  `crates/obscura-ext/src/runtime.rs::build_preload_script`.
- **Manifest quirks** — `crates/obscura-ext/src/manifest.rs`. The
  MV2/MV3 split and the `self_hosted` detection (via `update_url`)
  matter for any extension that gates on its version string.
- **Bundle loading** — `crates/obscura-ext/src/bundle.rs`. CRX2/CRX3
  header stripping; single-wrapper-directory unwrap for
  `*-master.zip`-style archives.
- **Bootstrap.js stubs** — `crates/obscura-js/js/bootstrap.js`. The
  `class NamedNodeMap`, `class Attr`, … block; the `DOMParser`
  class definition; the `Element.prototype.text` /
  `Element.prototype.querySelector` lines in the Element class.
- **Page-side injection** — `crates/obscura-browser/src/page.rs`,
  `Page::init_js`. The `if let Some(ext) = &self.context.extension`
  block runs after V8 is fresh and before any page script.
- **CLI** — `crates/obscura-cli/src/main.rs`. `--extension <PATH>`
  parsing and the `load_extension_runtime` helper.
- **Element-scoped querySelector** —
  `crates/obscura-dom/src/selector.rs::query_selector_within`.
- **Text extractor whitespace** —
  `crates/obscura-cli/src/main.rs::extract_readable_text`.

## License

Apache 2.0, same as the rest of Obscura. The extension bundles you
load with `--extension` carry their own licenses — respect them.
