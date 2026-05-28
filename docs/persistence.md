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
| `localStorage` | yes | `localstorage.json`. Per-origin (`scheme://host[:port]`). |
| `sessionStorage` | no  | Real browsers don't persist it either. In-memory per V8 isolate. |
| IndexedDB / `caches` | no | Bigger surface (B-trees, cursors, transactions). On the roadmap. |
| Service workers | no | Not emulated. |
| `chrome.storage.local` (extensions) | no | Stub remains in-memory per page. |

`sessionStorage` not persisting is the correct behaviour: it's
explicitly scoped to "this browsing context". The README has the same
language under *Browser extensions*.

## On-disk layout

```
$STORAGE_DIR/
  cookies.json
  localstorage.json
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

### `localstorage.json`

JSON object keyed by canonical origin (`scheme://host[:port]`), each
value an object of `{ key: value }` string pairs. The origin form
matches `url::Origin::ascii_serialization()` — default ports are
elided (`https://example.com` not `https://example.com:443`).

```json
{
  "https://gooseup.me": {
    "user_pref": "dark",
    "last_seen": "2026-05-28T15:42:00Z"
  },
  "https://twitter.com": {
    "device_id": "abc123"
  }
}
```

Opaque origins (`about:blank`, `data:` URIs, unparseable URLs) are
never written, matching browser behaviour where those pages don't get
a "real" localStorage either.

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

### `localStorage`

`obscura_net::LocalStorageStore` is an `RwLock<HashMap<origin,
HashMap<key, value>>>` (see
[`crates/obscura-net/src/localstorage.rs`][ls]). The bootstrap shim
in `crates/obscura-js/js/bootstrap.js` exposes
`globalThis.localStorage` as a `Proxy` over a `Storage.prototype`
core whose methods dispatch to six V8 ops:

```
op_localstorage_get_item(key) -> JSON("null" | string)
op_localstorage_set_item(key, value)
op_localstorage_remove_item(key)
op_localstorage_clear()
op_localstorage_length() -> u32
op_localstorage_key(index) -> JSON("null" | string)
```

Each op reads the current page URL from `ObscuraState.url`, derives
the canonical origin via `url::Origin::ascii_serialization()`, and
routes through the per-`BrowserContext` `Arc<LocalStorageStore>`.
The Proxy wraps the core so property access (`localStorage.foo`,
`'foo' in localStorage`, `delete localStorage.foo`,
`Object.keys(localStorage)`) all work.

[ls]: ../crates/obscura-net/src/localstorage.rs

### Why a `Proxy` (and not plain methods)

Real-world sites do `localStorage.foo = 'bar'` and
`for (const k of Object.keys(localStorage))`. Without a `Proxy`
those don't dispatch through `getItem` / `setItem` — they hit the
underlying JS object directly. With the `Proxy` the `get`, `set`,
`has`, `deleteProperty`, `ownKeys`, and `getOwnPropertyDescriptor`
traps all route through the ops while the method names (`getItem`,
`setItem`, etc.) are pass-through.

## Example: logged-in scrape

```bash
# Run 1: log in. The form submits via op_navigate (POST), the
# server's Set-Cookie response lands in the jar, and the CLI
# drains pending navigation before save_session() runs.
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

## Compatibility note

The existing `BrowserContext::save_cookies()` API is now a
`#[deprecated]` alias for `save_session()`, which writes both files.
External callers keep compiling; new code should prefer the new
name.

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
