# Persistent fetch worker protocol

This document specifies the private protocol between Obscura and process-local
clients such as the minimal-agent Fetch plugin. It is not a browser automation
API and is not exposed to the model.

## Goals

- Pay process and V8 initialization cost lazily, once per agent process.
- Keep named session cookies and local storage live between Fetch calls.
- Preserve the existing one-shot `obscura fetch` command as a fallback.
- Make hangs, crashes, oversized input, and protocol skew fail closed.
- Keep Obscura's single-live-isolate invariant.

## Transport

The worker is `obscura-worker --fetch-protocol`. Its stdin and stdout use
newline-delimited JSON. One line is one complete UTF-8 message. Diagnostics go
to stderr only. No log, banner, or page byte is ever written outside a protocol
response on stdout.

Limits:

- request line: 1 MiB;
- request id: non-empty string, at most 128 bytes;
- URL: at most 4096 bytes and `http` or `https` only;
- timeout: 1 through 120 seconds;
- selector: at most 64 KiB;
- eval expression: at most 1 MiB;
- response body: at most 64 MiB before base64;
- at most 8 named sessions resident in memory;
- one request executes at a time;
- worker idle shutdown defaults to 300 seconds and is configurable at worker
  startup, not per request.

The sequential execution rule is deliberate. A suspended page retains its DOM
but not its JavaScript heap or pending timers. Only the active session owns a
live V8 isolate. Switching sessions suspends the previous page and resumes the
next page with a fresh JS realm bound to its retained DOM. Cookies and
localStorage remain live because they belong to the session's BrowserContext.

## Version negotiation

Every request carries `"v":1`. An unsupported version receives a normal error
response with code `unsupported_version`; the worker stays alive.

The client starts with `hello`. A successful response reports protocol version,
worker PID, limits, and supported operations. The client must not send `fetch`
until `hello` succeeds.

```json
{"v":1,"id":"1","op":"hello"}
{"v":1,"id":"1","ok":true,"result":{"protocol":1,"pid":1234,"operations":["hello","fetch","status","close_session","shutdown"],"max_response_bytes":67108864,"max_sessions":8}}
```

## Fetch

```json
{
  "v": 1,
  "id": "2",
  "op": "fetch",
  "params": {
    "url": "https://example.com",
    "format": "markdown",
    "wait_until": "domcontentloaded",
    "timeout_ms": 30000,
    "settle_ms": 5000,
    "selector": null,
    "eval": null,
    "session": null,
    "storage_dir": null
  }
}
```

`format` is `html`, `text`, `links`, `markdown`, or `accessibility`.
`accessibility` returns `{ "nodes": [...] }` using the same CDP-compatible
AXNode records as `Accessibility.getFullAXTree`. Node IDs are document-scoped
and derived from DOM node IDs; synthetic bounds are explicitly marked.
`original` remains on the
one-shot path because it is binary-safe already and gains nothing from a V8
worker.

A named request contains both a validated session name and its absolute,
plugin-sandboxed storage directory. The worker treats the directory as opaque.
The client must constrain it below its configured storage root. An unnamed
request is ephemeral: it gets fresh cookies/localStorage and is destroyed after
the response. Named sessions reuse one BrowserContext and Page in memory and
flush both stores after every request, on explicit close, on eviction, and on
graceful shutdown.

Successful text output is UTF-8 in `body`. Binary output, when introduced, uses
`body_base64` with an explicit `encoding`; protocol JSON never carries arbitrary
bytes.

```json
{"v":1,"id":"2","ok":true,"result":{"url":"https://example.com/","title":"Example Domain","format":"markdown","body":"# Example Domain","bytes":16,"session":null}}
```

Errors are stable, machine-readable codes plus operator-facing details. Backend
details remain hidden by the plugin before a tool result reaches the model.

```json
{"v":1,"id":"2","ok":false,"error":{"code":"navigation_timeout","message":"navigation exceeded 30000ms"}}
```

Initial codes are `invalid_request`, `unsupported_version`, `unsupported_op`,
`invalid_url`, `invalid_params`, `navigation_failed`, `navigation_timeout`,
`eval_failed`, `selector_timeout`, `output_too_large`, and `internal`.

## Status and lifecycle

`status` returns PID, uptime, completed/failed request counts, resident named
sessions, active session, and configured limits. It never returns cookies,
localStorage, page content, proxy credentials, or absolute storage paths.

`close_session` saves and destroys one named session. `shutdown` saves and
destroys all sessions, emits its response, flushes stdout, and exits zero.
EOF or a broken stdout pipe performs the same best-effort cleanup. The parent
also owns a process-group kill fallback because graceful cleanup cannot be
assumed after a native hang.

The plugin starts the worker only on the first eligible rendered Fetch call.
If the active backend config includes WebExtensions, it passes the first bundle
path at worker startup and every worker-created BrowserContext shares that
loaded extension runtime, matching the one-shot CLI's current first-extension
behavior. It performs `hello`, correlates responses by id, and sends only one request at a
time. A caller abort or outer watchdog kills the whole worker process group;
the next request starts a clean worker. There is no in-band cancellation in v1:
a synchronous native op cannot reliably observe it, so pretending otherwise
would weaken the hard process boundary.

The worker may be restarted once after EOF, malformed stdout, or a protocol
error only when the failed request is known not to have begun. A dispatched
`fetch` is never automatically replayed because navigation or eval may have
mutated server and session state.

## Fallback

The plugin uses the existing one-shot backend when:

- persistent mode is disabled;
- the configured backend is not Obscura;
- format is `original`;
- worker startup or `hello` reports an unsupported protocol;
- the installed Obscura build predates `--fetch-protocol`.

A crash or timeout after a fetch was dispatched is returned as an error, not
silently replayed one-shot. The next independent call may use a restarted
worker. This preserves at-most-once behavior for side-effecting evals.

## Implementation slices

1. Move rendered-fetch execution and dump helpers from the CLI entry point into
   an internal CLI module shared by one-shot fetch and worker mode.
2. Replace the legacy worker loop with versioned protocol types, bounded line
   reads, validation, sequential dispatch, session activation, eviction, and
   graceful save/close.
3. Add plugin config for persistent mode and idle timeout, then a module-owned
   worker client with lazy start, hello, correlation, process-group cleanup,
   output bounds, and status reporting. Keep `callBackend` intact as fallback.
4. Add accessibility output only after this transport is stable. It should be a
   new format and use navigation-scoped identifiers; it must not overload the
   v1 text formats.

## Test matrix

Obscura unit tests:

- request/version/operation/field validation and every size/range boundary;
- stdout contains protocol JSON only and errors stay on stderr;
- output cap rejects before allocating an unbounded base64/string response;
- session LRU never exceeds the configured cap and eviction saves state;
- activation leaves exactly one live JS isolate;
- ephemeral calls do not share cookies or localStorage;
- named calls share in-memory state and persist it across worker restart;
- close, shutdown, EOF, and idle timeout save and release sessions;
- malformed input does not terminate the worker;
- a second request succeeds after a bounded eval timeout.

Plugin unit tests:

- worker is not spawned during plugin load or an `original` call;
- concurrent handler calls serialize through one process and correlate ids;
- abort/watchdog kills the process group and rejects pending calls once;
- EOF before dispatch may restart; EOF after dispatch never replays;
- protocol skew and unavailable worker fall back one-shot;
- named sessions map only to sandboxed storage directories;
- response and stderr buffers are capped;
- parent exit, explicit shutdown, and idle exit leave no child process;
- model-facing errors and display remain backend-agnostic;
- worker PID/status appears only in operator UI or diagnostics.

Integration and release gates:

- cold rendered Fetch, warm ephemeral Fetch, warm named-session Fetch, and
  one-shot timings;
- cookie and localStorage continuity over two calls and one worker restart;
- forced worker crash and forced infinite eval recovery;
- `cargo build --release -p obscura-cli`;
- `cargo nextest run -p obscura-cli -p obscura-browser`;
- plugin package tests and repository `bun run check`;
- stealth build/path validation;
- obstacle course remains 33/33.
