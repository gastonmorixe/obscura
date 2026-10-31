# Persistent sessions

Obscura keeps a browser session alive across runs when `--storage-dir
<DIR>` is set on `obscura fetch`, `obscura serve`, or `obscura mcp`.
The directory holds the cookie jar and per-origin `localStorage`,
loaded on context creation and saved on graceful shutdown. Re-running
against the same directory comes back logged in.

## What gets persisted

| Surface | Persisted? | Why / why not |
| --- | --- | --- |
| HTTP cookies (jar) | yes | `cookies.json`. Domain-scoped, same wire format as a Chrome cookie export. |
| `localStorage` | yes | One file per origin under `localStorage/`. Keyed by `scheme://host[:port]`. |
| `sessionStorage` | no  | Real browsers don't persist it either. Lives on the `Page`, keyed by origin, so it survives a same-origin navigation and dies with the tab. |
| IndexedDB | no  | In memory for the context lifetime. Not written to disk. |
| `caches` | no | Not implemented. |
| Service workers | no | Not emulated. |
| `chrome.storage.local` (extensions) | no | Stub remains in-memory per page. |

`sessionStorage` not persisting is the correct behaviour: it's
explicitly scoped to "this browsing context". The README has the same
language under *Browser extensions*.

## On-disk layout

```
$STORAGE_DIR/
  cookies.json
  localStorage/
    https___example.com-837b2b5793a240b3.json
```

### `cookies.json`

JSON array of `{ name, value, domain, path, secure, httpOnly,
sameSite, expires }` records. Same shape as
`document.cookie`-style exports. Expired cookies are dropped on save.

```json
[
  {
    "name": "connect.sid",
    "value": "s%3A...",
    "domain": "gooseup.me",
    "path": "/",
    "secure": false,
    "httpOnly": true,
    "sameSite": "Lax",
    "expires": 1811448402
  }
]
```

### `localStorage/<stem>.json`

One file per origin. The stem is a readable prefix plus a 64-bit FNV-1a
digest of the origin, so two origins that sanitize to the same prefix
cannot share a file.

```json
{
  "origin": "https://gooseup.me",
  "entries": [
    ["user_pref", "dark"],
    ["last_seen", "2026-05-28T15:42:00Z"]
  ]
}
```

`entries` is an ordered list because `Storage.key(i)` depends on insertion
order. The origin is stored inside the file and validated on load: it must
match `url::Origin::ascii_serialization()`, or be `file://`.

Opaque origins (`about:blank`, `data:` URIs, unparseable URLs) are never
written. Reading `localStorage` there throws `SecurityError`, as in a real
browser.

### Upgrading from the single-file layout

Profiles written before the per-origin split hold one
`$STORAGE_DIR/localstorage.json` mapping `{ origin: { key: value } }`.
On load, if that file is present it is adopted into the per-origin store
once, with an `info!` log line, and left on disk so a downgrade still finds
its data.

## Write semantics

Both files are written **atomically**: a `NamedTempFile` in the same
directory, populated, then `persist()`-renamed over the target. A
half-written save can't leave the file partially overwritten.

Writes happen at a **single point per run**:

- `obscura fetch` — after the eval or dump returns, before the
  process exits.
- `obscura serve` — when the CDP processor loop exits (Ctrl-C or
  the accept channel closing).
- `obscura mcp` — when the client calls `browser_close`.

A crash, SIGKILL, or panic before that point loses any in-session
mutations. For high-value sessions a `Drop` impl is on the roadmap;
for v1 the graceful paths above are the contract.

## How it works under the hood

### Cookies

`obscura_net::CookieJar` is an `RwLock<HashMap<domain, HashMap<name,
entry>>>`. The HTTP client (`obscura_net::ObscuraHttpClient`) holds
an `Arc<CookieJar>`, applies the `Cookie:` header on every request,
and writes `Set-Cookie` responses back. `document.cookie` getter and
setter go through `op_get_cookies` / `op_set_cookie` (which dispatch
to the same jar's `get_js_visible_cookies` / `set_cookie_from_js`).

### `localStorage` and `sessionStorage`

`obscura_js::ops::OriginStorage` holds one origin's entries behind a lock,
keyed by canonical origin. The context owns the `localStorage` instance, so it
outlives a V8 realm. The `Page` owns `sessionStorage`, so it survives a
same-origin navigation and dies with the tab.

`bootstrap.js` exposes both as a `Proxy` over a `Storage.prototype` core. The
core reads and writes through the host bridge, so a property write
(`localStorage.theme = "dark"`) and `setItem` take the same path. A write to an
opaque origin throws `SecurityError` rather than silently doing nothing.

## Example: logged-in scrape

```bash
# Run 1: log in. The form submits via op_navigate (POST), the
# server's Set-Cookie response lands in the jar, and the CLI
# drains pending navigation before save_storage() runs.
obscura fetch https://gooseup.me/ --storage-dir ./session --eval '
  const f = document.querySelector("form[action=\"/auth/login\"]");
  f.querySelector("input[name=\"username\"]").value = "gaston";
  f.querySelector("input[name=\"password\"]").value = "29422942...!!";
  f.submit();
  "ok"
'

# Run 2: same dir, brand-new process. Cookies + localStorage are
# loaded on BrowserContext construction. Page renders as logged in.
obscura fetch https://gooseup.me/ --storage-dir ./session --eval '
  document.querySelector("header, nav")?.textContent?.trim()
'
# → "@gaston  Settings Logout"
```

This is exercised end-to-end by
[`crates/obscura-cdp/tests/session_persistence.rs`][test], which
spins up an in-process HTTP server (no network) and asserts the
round-trip across two separate `CdpContext` instances.

[test]: ../crates/obscura-cdp/tests/session_persistence.rs

## Threat model

- **Plaintext on disk.** Sites stash auth tokens in cookies and
  `localStorage`. Anyone with read access to the directory has
  read access to those tokens. Protect it the way you'd protect a
  Chrome profile dir — file permissions, an encrypted volume,
  whatever your platform offers.
- **Single-writer.** No file locking. Two `obscura` processes
  pointed at the same `--storage-dir` will race and one will lose.
  If you need a shared session, run one obscura instance and have
  the other processes talk to it over CDP.
- **HttpOnly cookies are still on disk.** The `httpOnly` flag means
  JS can't see the cookie via `document.cookie`. It does NOT mean
  the cookie isn't in the file you handed obscura.

## Flushing to disk

`BrowserContext::save_storage()` writes `localStorage` and cookies.
`save_local_storage()` and `save_cookies()` write one half each. The CLI calls
`save_storage()` on every exit path that owns a context, and the CDP server and
MCP `browser_close` do the same. Only origins whose entries changed since the
last flush are rewritten.

## Deferred / known gaps

- **IndexedDB and `caches` API persistence.** Stubs remain in
  `bootstrap.js`. Most cookie + localStorage sites work without
  them.
- **Save on mutation.** Currently save-on-exit. A SIGKILL between
  the last mutation and shutdown loses state. A periodic flush
  (every N seconds, or after N writes) is a worthwhile follow-up.
- **Cross-process locking.** Single-writer is fine for the typical
  CLI / one-server flow; a `.lock` file would let multiple
  instances cooperate.
- **Encryption at rest.** No knob to encrypt the JSON. Run on an
  encrypted volume if it matters.
