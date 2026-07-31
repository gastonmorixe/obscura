# X login JavaScript investigation

This report documents the investigation of Obscura loading
`https://x.com/i/flow/login`, the misleading symptoms observed during the
investigation, the fixes made, the limits that remain, and the recommended way
to debug similar JavaScript-heavy applications.

The investigation was performed on 2026-07-28 with the release binary built
from this repository.

## Executive summary

Obscura successfully downloads X's login document and executes its core
JavaScript bundles. The page is not failing because JavaScript is globally
disabled.

Three distinct behaviors originally looked like one failure:

1. The text `JavaScript is not available` came from an inert `<noscript>`
   subtree. Obscura's text extractor included that subtree, so a text dump made
   the page look as if X had detected disabled JavaScript.
2. X's core runtime, vendor, localization, and main bundles did execute. Runtime
   probes showed `window.__SCRIPTS_LOADED__.runtime`, `vendor`, and `main` set to
   `true`.
3. X then created a large graph of dynamic webpack chunks. Obscura serialized
   every dynamically-created classic script through the same queue used to
   protect ES-module loading. The queue accumulated 14 to 32 scripts in the
   observed runs, and the page did not consistently finish rendering the login
   form before a bounded event-loop watchdog fired.

The implemented fixes now:

- exclude `<noscript>` from `--dump text`;
- fetch dynamically-created classic scripts concurrently;
- keep ES-module imports serialized, as required by the current `deno_core`
  runtime integration;
- report blocked, non-success HTTP, or failed classic dynamic scripts through
  the script element's `error` event instead of incorrectly firing `load`;
- count concurrent classic dynamic script requests as pending work; and
- prevent the post-script settle loop from declaring idle while JavaScript's
  dynamic loader still has work.

After these fixes, X advances further and the misleading no-JavaScript text no
longer appears in text dumps. The complete username input still does not render
reliably. Some dynamic requests remain pending until the bounded settle
watchdog fires, so more browser-compatibility work is required.

## Reproduction

The `fetch` subcommand is required. A command with global flags but no
subcommand is parsed as a top-level CLI invocation and does not accept fetch
options such as `--wait-until`.

Minimal rendered fetch:

```bash
./target/release/obscura fetch \
  --stealth \
  --wait-until domcontentloaded \
  'https://x.com/i/flow/login'
```

Useful diagnostic fetch:

```bash
./target/release/obscura fetch \
  --stealth \
  --timeout 60 \
  --wait 0 \
  --wait-until domcontentloaded \
  --eval '(function(){
    return JSON.stringify({
      readyState: document.readyState,
      loaded: window.__SCRIPTS_LOADED__ || null,
      failures: window.__SCRIPT_LOAD_FAILURE__?.failures || [],
      inputs: Array.from(document.querySelectorAll("input")).map(function(el) {
        return {
          name: el.name,
          type: el.type,
          placeholder: el.placeholder,
          autocomplete: el.autocomplete
        };
      }),
      buttons: Array.from(document.querySelectorAll("button")).map(function(el) {
        return (el.innerText || el.textContent || "").trim();
      })
    });
  })()' \
  'https://x.com/i/flow/login'
```

A multi-statement expression is wrapped in an IIFE because a V8 script whose
completion value comes from a declaration such as `const` can produce `null`.

## What was observed

### Navigation and the initial document

The navigation completed with exit status 0 and produced approximately 299 KB
of serialized HTML in one observed run. The parsed document contained 43
`<script>` elements after page activity.

The initial X document included:

- an inert `<noscript>` fallback containing `JavaScript is not available`;
- a `#react-root` application mount point;
- inline bootstrap state and feature configuration;
- the webpack runtime;
- deferred vendor, localization, and main bundles; and
- a `ScriptLoadFailure` fallback controlled by X's own bootstrap logic.

These pieces must be evaluated separately. The presence of `<noscript>` text in
serialized HTML does not indicate that the browser displayed it.

### Core JavaScript execution

With browser and runtime logging enabled, Obscura fetched and executed X's
large core bundles. The exact content hashes vary as X deploys new builds, but
the sequence included resources equivalent to:

```text
vendor.<hash>.js       approximately 626 KB
 i18n/en.<hash>.js     approximately 656 KB
 main.<hash>.js        approximately 1.0 MB
```

A post-navigation runtime probe returned:

```json
{
  "runtime": true,
  "vendor": true,
  "main": true
}
```

X's script failure registry was empty in those runs. This proves that V8 ran
page JavaScript and that the core X application bundles reached their own
loaded markers.

### Misleading `<noscript>` extraction

The old `--dump text` path walked the DOM and skipped `script`, `style`, and
several boilerplate elements, but it did not skip `noscript`. As a result, the
output contained:

```text
JavaScript is not available.
We've detected that JavaScript is disabled in this browser.
```

The same issue affected probes based on Obscura's current `innerText`
implementation, because `Element.innerText` is currently an alias for
`textContent` in `bootstrap.js`. It does not perform layout-aware visibility
filtering.

The reliable checks were instead:

- `window.__SCRIPTS_LOADED__`;
- live form controls and their attributes;
- X's script failure registry;
- the dynamic script queue state; and
- explicit DOM inspection of the mounted application subtree.

After the text extraction fix, `--dump text` omits the inert fallback. Depending
on timing, observed real output included either X's application error:

```text
Something went wrong, but don't fret. Let's give it another shot.

Try again

Some privacy related extensions may cause issues on x.com.
Please disable them and try again.
```

or a partial application state such as:

```text
Happening now.
```

### Dynamic webpack chunk backlog

X's main bundle inserts many additional classic scripts at runtime. Before the
loader change, all dynamic scripts were placed in one serialized queue. The
queue existed to prevent concurrent ES-module imports from re-entering
`deno_core`'s module loader, but it also serialized ordinary classic scripts
that browsers normally fetch asynchronously.

Probes observed states such as:

```json
{
  "dynBusy": true,
  "dynQueue": 16
}
```

and later runs reached queue lengths above 30. Increasing only the CLI timeout
did not fix this. The application was still fetching and evaluating chunks one
at a time.

After classic scripts were separated from the module queue:

- `dynQueue` could drain to zero;
- module loading remained serialized;
- concurrently fetching classic scripts were counted in `dynPending`; and
- the event-loop settle logic waited on both forms of work.

One extended run reached:

```json
{
  "dynBusy": false,
  "dynQueue": 0,
  "dynPending": 8
}
```

before the settle watchdog terminated the remaining work. This is an
improvement, but it also shows that the current remaining failure is broader
than queue serialization alone.

## Relevant architecture

### CLI fetch ordering

`crates/obscura-cli/src/main.rs` performs a rendered fetch in this order:

1. construct a `BrowserContext` and `Page`;
2. parse `--wait-until`;
3. run `Page::navigate_with_wait` under the CLI navigation timeout;
4. call `Page::settle(--wait)`;
5. evaluate `--eval`, if supplied;
6. when `--eval` is combined with an explicit `--dump` or `--selector`, settle
   again;
7. poll for `--selector`, if supplied; and
8. produce the selected dump format.

A bare `--eval`, with neither explicit `--dump` nor `--selector`, returns the
evaluation result directly. It does not perform the second post-evaluation
settle.

### Parser-discovered script loading

`Page::execute_scripts` in `crates/obscura-browser/src/page.rs`:

1. snapshots parser-discovered scripts from the DOM;
2. divides them into regular, deferred, async, and module groups;
3. fetches classic external scripts concurrently;
4. executes regular, deferred, and async groups in that grouped order;
5. loads module scripts;
6. synthesizes lifecycle events; and
7. pumps a bounded event loop for dynamically-created resources.

The whole script phase has a soft deadline and a V8 termination watchdog. Each
classic script is also executed through a separate guarded call.

### Dynamic script loading

`crates/obscura-js/js/bootstrap.js` implements dynamic script behavior because
V8 does not provide a browser DOM loader itself.

Before this investigation, both classic scripts and modules went through
`__dynScriptQueue`. The updated design has two paths:

- **classic scripts:** start `op_fetch_url` concurrently, evaluate when each
  response arrives, and dispatch `load` or `error` on the element;
- **module scripts:** remain in the serialized import queue to avoid reentrant
  module graph loading in `deno_core`.

`ObscuraJsRuntime::has_pending_dynamic_scripts` reports pending work from both
paths to the browser layer.

## Fixes implemented

### Exclude `<noscript>` from text dumps

File:

```text
crates/obscura-cli/src/main.rs
```

`extract_readable_text` now skips `noscript` alongside `script` and `style`.
The associated unit test verifies that active article text remains while
script source, styles, and the no-JavaScript fallback are excluded.

This fixes diagnosis and extraction quality. It does not alter page execution.

### Fetch dynamic classic scripts concurrently

File:

```text
crates/obscura-js/js/bootstrap.js
```

Dynamically-created classic scripts now use a separate concurrent loader.
Important behavior:

- the request starts immediately instead of waiting behind unrelated chunks;
- the script evaluates after its response arrives;
- `load` fires only after successful fetch and evaluation;
- blocked, non-success HTTP, fetch-failed, and evaluation-failed scripts fire
  `error`; and
- `__currentScriptNid` is restored after evaluation.

Modules still use the serialized queue.

### Track pending classic dynamic scripts

Files:

```text
crates/obscura-js/js/bootstrap.js
crates/obscura-js/src/runtime.rs
```

The JavaScript loader increments `__dynScriptPending` before a classic request
and decrements it in `finally`. `has_pending_dynamic_scripts` now checks:

- whether the module queue is busy;
- whether the module queue contains tasks; and
- whether concurrent classic script requests remain pending.

### Prevent false idle detection

File:

```text
crates/obscura-browser/src/page.rs
```

The bounded dynamic settle loop previously could break after two idle event-loop
polls whenever the Rust HTTP client reported zero active requests. That was not
sufficient because the JS-side dynamic loader can still have queued or pending
work.

The loop now consults `has_pending_dynamic_scripts` before incrementing its idle
counter.

### Test dynamic classic script concurrency

File:

```text
crates/obscura-cdp/tests/dynamic_script_onload_fires.rs
```

The regression test inserts two classic external scripts:

- one delayed by 600 ms;
- one delayed by 50 ms.

It verifies that both execute and fire `load`, and that the faster script's load
handler runs first. This detects accidental reintroduction of global classic
script serialization.

## Timeout and wait semantics

Several independent bounds apply. They should not be treated as one timeout.

### `--timeout`

The CLI applies `--timeout` around navigation and uses it for guarded
`--eval`. It does not automatically replace every internal browser deadline.

### `OBSCURA_NAV_TIMEOUT_MS`

`Page::navigate_with_wait` has its own navigation ceiling, defaulting to 30
seconds. Raising only `--timeout` above 30 seconds does not raise this internal
ceiling.

### `OBSCURA_SCRIPT_DEADLINE_MS`

The parser-discovered script phase defaults to 30 seconds. A phase watchdog is
armed slightly beyond this deadline.

### `OBSCURA_DYNAMIC_SCRIPT_SETTLE_MS`

The post-script dynamic settle budget defaults to 3 seconds, with a minimum of
500 ms. It is a bounded event-loop budget, not a guarantee that every application
chunk will complete.

### `OBSCURA_FETCH_TIMEOUT_MS`

Scripted `fetch`, XHR, and dynamic resource requests have their own request
timeout, defaulting to 30 seconds.

### `--wait`

`--wait` is a maximum post-navigation settle budget. `Page::settle` returns
early when the event loop reports idle. It is not equivalent to sleeping for an
exact number of seconds.

### `networkidle0` and `networkidle2`

The current network-idle implementation has a fixed five-second best-effort
window. It waits for at most zero or two active requests for 500 ms, but it
still marks the lifecycle as network idle when its five-second deadline is
reached.

For X and other long-lived SPAs, `domcontentloaded` plus an explicit selector is
usually easier to interpret than `networkidle0`.

## Recommended diagnostic command

For a heavy application, align the important internal budgets and inspect a
real application condition:

```bash
OBSCURA_NAV_TIMEOUT_MS=60000 \
OBSCURA_SCRIPT_DEADLINE_MS=60000 \
OBSCURA_DYNAMIC_SCRIPT_SETTLE_MS=10000 \
OBSCURA_FETCH_TIMEOUT_MS=30000 \
./target/release/obscura fetch \
  --stealth \
  --timeout 60 \
  --wait 10 \
  --wait-until domcontentloaded \
  --selector 'input[autocomplete="username"]' \
  --dump html \
  'https://x.com/i/flow/login'
```

Interpretation:

- successful navigation means the document and initial scripts loaded;
- a missing selector means the expected application state was not reached;
- a watchdog warning means a bounded V8 or event-loop phase exceeded its
  allowance, not necessarily that JavaScript was disabled; and
- the presence of `<noscript>` in an HTML dump is normal and does not mean it
  was displayed.

For an eval that starts asynchronous work and modifies the DOM, specify
`--dump` or `--selector` so the CLI performs its post-eval settle before reading
the page.

## Authentication workflow guidance

A one-shot `fetch` command is suitable for navigation, evaluation, extraction,
and simple scripted interactions. A multi-stage authentication flow is better
driven through the CDP server:

```bash
./target/release/obscura serve \
  --stealth \
  --storage-dir ./x-session \
  --port 9222
```

Connect Puppeteer or Playwright, complete the login steps, and reuse the stored
cookies and local storage in later sessions. This provides:

- multiple interactions in one live page;
- better inspection after each step;
- explicit waiting for form states;
- persistent session data; and
- access to request and console events through CDP.

The storage directory contains authentication material in plaintext and should
be protected accordingly.

## Remaining engine issues

The fixes in this investigation are intentionally narrow. The following issues
remain relevant to X and other large SPAs.

### Lifecycle events do not match browser phases

`Page::execute_scripts` currently sets `interactive`, dispatches
`DOMContentLoaded`, directly invokes `window.onload`, sets `complete`, and
dispatches `load` inside one function. This happens before the Rust navigation
layer decides whether the caller requested `domcontentloaded` or `load`.

The script and lifecycle phases should be separated so that:

1. parser-blocking scripts execute during parsing;
2. deferred classic scripts and modules execute before `DOMContentLoaded`;
3. `DOMContentLoaded` fires once with browser-compatible propagation;
4. asynchronous resource work continues; and
5. `load` fires only after the load phase completes.

### Parser-discovered script fetch timeout discards partial success

The classic script fetch phase uses a buffered concurrent stream but collects
the whole stream under one timeout. If the timeout expires, the current code
returns an empty result vector. Responses that completed before the deadline
are discarded together with stalled requests.

The loader should consume completed responses incrementally until the deadline
and retain all successes.

### Script failures are not typed precisely

The five-second per-script guard in `ObscuraJsRuntime::execute_script_guarded`
converts execution termination into `Ok(())`. Module graph fetch, evaluation,
and timeout paths also swallow some failures.

Callers need distinct outcomes for:

- successful evaluation;
- JavaScript exception;
- network failure;
- module graph failure; and
- watchdog termination.

Those outcomes should drive correct `load` and `error` events and provide useful
navigation diagnostics.

### Dynamic insertion coverage is incomplete

The dynamic loader is attached primarily to `appendChild`. Equivalent connected
insertions through `insertBefore`, `replaceChild`, or insertion of a subtree
containing script elements do not yet share one consistent connected-insertion
hook.

### CLI eval-triggered navigation needs verification

The CLI source and changelog describe draining navigation queued by an eval, but
the inspected `run_fetch` path should be kept covered by an integration test
that evaluates `location.href` or submits a form and verifies the final page
before output and session save.

### Timeout configuration is fragmented

A CLI timeout, navigation timeout, script deadline, per-script guard, dynamic
settle budget, fetch timeout, network-idle deadline, and process hard deadline
can all apply to one command. The CLI should eventually derive these from a
single operation budget or expose the major phase budgets directly.

Invalid `--wait-until` values should also be rejected rather than silently
falling back to `load`.

### Stealth and tracker blocking are coupled

`--stealth` enables fingerprint/TLS behavior and tracker blocking together. X
rendered a warning about privacy-related extensions in some runs. The same
application failure also occurred without stealth, so tracker blocking was not
proven to be the sole cause, but the features should be separable for diagnosis
and for sites whose application code depends on a blocked host.

A future `--no-block-trackers` or independent tracker policy would make this
possible without losing the stealth transport and browser identity.

### `innerText` is not layout-aware

`Element.innerText` currently returns `textContent`. It therefore includes
hidden and inert subtrees and cannot be used as a visibility oracle. A proper
implementation should account for element visibility, CSS display, inert
content, and block-level line boundaries.

## Suggested implementation order

The highest-value follow-up sequence is:

1. retain partial parser-script fetch successes at the phase deadline;
2. return typed classic and module script execution outcomes;
3. split `DOMContentLoaded` and `load` into real lifecycle phases;
4. unify connected dynamic-script insertion behavior;
5. consolidate or explicitly expose timeout budgets;
6. separate tracker blocking from stealth identity; and
7. implement layout-aware `innerText`.

Each change should include a deterministic local fixture. Avoid making X itself
the regression gate because its bundles, feature flags, and anti-abuse behavior
change independently of this repository.

## Validation performed

The focused source changes compiled and passed the supported test runner:

```text
cargo build --release -p obscura-cli
    Finished release profile
```

```text
cargo nextest run -p obscura-cli -p obscura-browser
    65 tests passed
```

```text
cargo nextest run -p obscura-cdp --test dynamic_script_onload_fires
    1 test passed
```

The authoritative obstacle course was not run during the investigation because
the companion `obscura-benchmark/obstacle-course/run.py` repository was not
present under the active project directory.

## Current status

At the end of this investigation:

- X's document loads;
- V8 executes the core runtime, vendor, localization, and main bundles;
- `--dump text` no longer reports inert `<noscript>` content;
- dynamic classic chunks fetch concurrently;
- pending dynamic work participates in idle detection;
- X advances farther than before in some runs; and
- the complete login form still does not render reliably before bounded pending
  work is terminated.

This is a partial compatibility improvement, not a claim that the X login flow
is fully supported.
