<p align="center">
  <img src="https://raw.githubusercontent.com/h4ckf0r0day/obscura/main/assets/icon.png" alt="Obscura" width="80" />
</p>

<h2 align="center">Obscura</h2>

<p align="center">
  <strong>The open-source headless browser for AI agents and web scraping.</strong><br>
  Lightweight, stealthy, and built in Rust.
</p>

---

Obscura is a headless browser engine written in Rust, built for web scraping and AI agent automation. It runs real JavaScript via V8, supports the Chrome DevTools Protocol, and acts as a drop-in replacement for headless Chrome with Puppeteer and Playwright.

### Why Obscura over headless Chrome?

Designed for automation at scale, not desktop browsing.

| Metric       | Obscura      | Headless Chrome |
|--------------|--------------|------------------|
| Memory       | **30 MB**    | 200+ MB          |
| Binary size  | **70 MB**    | 300+ MB          |
| Anti-detect  | **Built-in** | None          |
| Page load    | **85 ms**    | ~500 ms          |
| Startup      | **Instant**  | ~2s              |
| Puppeteer    | **Yes**      | Yes              |
| Playwright   | **Yes**      | Yes              |

## 🎉 10,000 stars and what's next

I'm working on **Obscura Cloud** the hosted version, with managed infrastructure, residential proxies, and dedicated support. For people who want the engine without operating it themselves.

The open-source engine stays Apache-2.0, fully featured. No feature gating, ever.

**[Get on the waitlist →](https://tally.so/r/gDWzdD)**

## Install

### Download

Grab the latest binary from [Releases](https://github.com/h4ckf0r0day/obscura/releases):

```bash
# Linux x86_64
curl -LO https://github.com/h4ckf0r0day/obscura/releases/latest/download/obscura-x86_64-linux.tar.gz
tar xzf obscura-x86_64-linux.tar.gz
./obscura fetch https://example.com --eval "document.title"

# Linux ARM64 (aarch64)
curl -LO https://github.com/h4ckf0r0day/obscura/releases/latest/download/obscura-aarch64-linux.tar.gz
tar xzf obscura-aarch64-linux.tar.gz

# Arch Linux (AUR)
yay -S obscura-browser

# macOS Apple Silicon
curl -LO https://github.com/h4ckf0r0day/obscura/releases/latest/download/obscura-aarch64-macos.tar.gz
tar xzf obscura-aarch64-macos.tar.gz

# macOS Intel
curl -LO https://github.com/h4ckf0r0day/obscura/releases/latest/download/obscura-x86_64-macos.tar.gz
tar xzf obscura-x86_64-macos.tar.gz

# Windows
Download the `.zip` from the releases page and extract it manually.
```

No Chrome, no Node.js, no dependencies. Release archives include both
`obscura` and `obscura-worker`; keep them in the same directory for the
parallel `scrape` command.

Linux release builds target Ubuntu 22.04 so the downloaded binary remains
usable on common LTS servers with glibc 2.35+.

### Docker

```bash
docker run -d --name obscura -p 127.0.0.1:9222:9222 h4ckf0r0day/obscura
```

Image on [Docker Hub](https://hub.docker.com/r/h4ckf0r0day/obscura). Multi-stage build on `distroless/cc`, no shell, no package manager, ~57 MB compressed.

### Build from source

```bash
git clone https://github.com/h4ckf0r0day/obscura.git
cd obscura
cargo build --release

# With stealth mode (anti-detection + tracker blocking)
cargo build --release --features stealth
```

Requires Rust 1.75+ ([rustup.rs](https://rustup.rs)). First build takes ~5 min (V8 compiles from source, cached after).

## Quick Start

### Fetch a page

```bash
# Get the page title
obscura fetch https://example.com --eval "document.title"

# Extract all links
obscura fetch https://example.com --dump links

# Render JavaScript and dump HTML
obscura fetch https://news.ycombinator.com --dump html

# Write dump or eval output to a file
obscura fetch https://example.com --dump text --output page.txt

# Stream the raw response body verbatim (binary-safe; bypasses the JS/DOM layer).
# Use this for images, JSON, JS, CSS, or any non-HTML resource.
obscura fetch https://picsum.photos/200/300 --dump original > photo.jpg

# List every sub-resource URL the page would fetch (NDJSON; one record per asset)
obscura fetch https://example.com --dump assets

# Fetch through an HTTP or SOCKS proxy
obscura --proxy socks5://127.0.0.1:1080 fetch https://example.com --dump text

# Wait for dynamic content
obscura fetch https://example.com --wait-until networkidle0

# Bound navigation time for slow or broken pages
obscura fetch https://example.com --timeout 10
```

### Start the CDP server

```bash
obscura serve --port 9222

# With stealth mode (anti-detection + tracker blocking)
obscura serve --port 9222 --stealth
```

### Scrape in parallel

```bash
obscura scrape url1 url2 url3 ... \
  --concurrency 25 \
  --eval "document.querySelector('h1').textContent" \
  --format json

# Suppress scrape progress on stderr for script-friendly output
obscura scrape https://example.com --quiet --format json

# Scrape workers inherit the global proxy
obscura --proxy http://127.0.0.1:8080 scrape https://example.com https://news.ycombinator.com
```

## Puppeteer / Playwright

### Puppeteer

```bash
npm install puppeteer-core
```

```javascript
import puppeteer from 'puppeteer-core';

const browser = await puppeteer.connect({
  browserWSEndpoint: 'ws://127.0.0.1:9222/devtools/browser',
});

const page = await browser.newPage();
await page.goto('https://news.ycombinator.com');

const stories = await page.evaluate(() =>
  Array.from(document.querySelectorAll('.titleline > a'))
    .map(a => ({ title: a.textContent, url: a.href }))
);
console.log(stories);

await browser.disconnect();
```

### Playwright

```bash
npm install playwright-core
```

```javascript
import { chromium } from 'playwright-core';

const browser = await chromium.connectOverCDP({
  endpointURL: 'ws://127.0.0.1:9222',
});

const page = await browser.newContext().then(ctx => ctx.newPage());
await page.goto('https://en.wikipedia.org/wiki/Web_scraping');
console.log(await page.title());

await browser.close();
```

### Form submission & login

```javascript
await page.goto('https://quotes.toscrape.com/login');
await page.evaluate(() => {
  document.querySelector('#username').value = 'admin';
  document.querySelector('#password').value = 'admin';
  document.querySelector('form').submit();
});
// Obscura handles the POST, follows the 302 redirect, maintains cookies
```

## Benchmarks

Page load:

| Page | Obscura | Chrome |
|------|---------|--------|
| Static HTML | **51 ms** | ~500 ms |
| JS + XHR + fetch | **84 ms** | ~800 ms |
| Dynamic scripts | **78 ms** | ~700 ms |

## Stealth Mode

Enable with `--features stealth`.

### Anti-fingerprinting
- Per-session fingerprint randomization (GPU, screen, canvas, audio, battery)
- Realistic `navigator.userAgentData` (Chrome 145, high-entropy values)
- `event.isTrusted = true` for dispatched events
- Hidden internal properties (`Object.keys(window)` safe)
- Native function masking (`Function.prototype.toString()` → `[native code]`)
- `navigator.webdriver = undefined` (matches real Chrome)

### Tracker Blocking
- 3,520 domains blocked
- Blocks analytics, ads, telemetry, and fingerprinting scripts
- Prevents trackers from loading entirely
- Enabled automatically with `--stealth`

## CDP API

Obscura implements the Chrome DevTools Protocol for Puppeteer/Playwright compatibility.

| Domain | Methods |
|--------|---------|
| **Target** | createTarget, closeTarget, attachToTarget, createBrowserContext, disposeBrowserContext |
| **Page** | navigate, getFrameTree, addScriptToEvaluateOnNewDocument, lifecycleEvents |
| **Runtime** | evaluate, callFunctionOn, getProperties, addBinding |
| **DOM** | getDocument, querySelector, querySelectorAll, getOuterHTML, resolveNode |
| **Network** | enable, setCookies, getCookies, setExtraHTTPHeaders, setUserAgentOverride |
| **Fetch** | enable, continueRequest, fulfillRequest, failRequest (live interception) |
| **Storage** | getCookies, setCookies, deleteCookies |
| **Input** | dispatchMouseEvent, dispatchKeyEvent |
| **LP** | getMarkdown (DOM-to-Markdown conversion) |
## CLI Reference

### Global flags

| Flag | Description |
|------|-------------|
| `--proxy <URL>` | HTTP/SOCKS5 proxy applied to every subcommand |
| `--user-agent <UA>` | Custom User-Agent string |
| `--v8-flags <FLAGS>` | Raw V8 flags (see *Tuning V8* below) |
| `--extension <PATH>` | Load a WebExtension bundle (folder, `.zip`, `.xpi`, or `.crx`). See *Browser extensions* below |
| `--obey-robots` | Respect `robots.txt` |
| `-v`, `--verbose` | More chatty `tracing` output (also via `RUST_LOG=`) |

### Tuning V8

Obscura embeds V8 directly. Use `--v8-flags` to pass raw flags through to V8, same syntax as Chromium's `--js-flags` and Node's command-line flags. Most common use is raising the heap cap to fix `JavaScript heap out of memory` on JS-heavy pages:

```bash
obscura --v8-flags "--max-old-space-size=4096" fetch <url>
```

### `obscura serve`

Start a CDP WebSocket server.

| Flag | Default | Description |
|------|---------|-------------|
| `--port` | `9222` | WebSocket port |
| `--proxy` | — | HTTP/SOCKS5 proxy URL |
| `--stealth` | off | Enable anti-detection + tracker blocking |
| `--workers` | `1` | Number of parallel worker processes |
| `--obey-robots` | off | Respect robots.txt |

### `obscura fetch <URL>`

Fetch and render a single page.

| Flag | Default | Description |
|------|---------|-------------|
| `--dump` | `html` | Output: `html`, `text`, `links`, `markdown`, `assets` (NDJSON of every sub-resource URL the page references), or `original` (raw response body) |
| `--eval` | — | JavaScript expression to evaluate |
| `--wait-until` | `load` | Wait: `load`, `domcontentloaded`, `networkidle0` |
| `--timeout` | `30` | Maximum navigation time in seconds |
| `--selector` | — | Wait for CSS selector |
| `--stealth` | off | Anti-detection mode |
| `--output` | — | Write dump or eval output to a file |
| `--quiet` | off | Suppress banner |
| `--proxy` | — | Inherited global HTTP/SOCKS5 proxy URL |

### `obscura scrape <URL...>`

Scrape multiple URLs in parallel with worker processes.

| Flag | Default | Description |
|------|---------|-------------|
| `--concurrency` | `10` | Parallel workers |
| `--eval` | — | JS expression per page |
| `--format` | `json` | Output: `json` or `text` |
| `--quiet` | off | Suppress scrape progress on stderr |
| `--proxy` | — | Inherited global HTTP/SOCKS5 proxy URL for all workers |

## MCP (Model Context Protocol)

Obscura ships an MCP server that exposes browser automation tools to AI agents (Claude Desktop, Cursor, etc.).

### Start

**stdio** (default) — for Claude Desktop and MCP clients that launch a subprocess:

```bash
obscura mcp
```

**HTTP** — for clients that connect over the network:

```bash
obscura mcp --http --port 8080
# endpoint: http://127.0.0.1:8080/mcp
```

Optional flags (both transports):

| Flag | Description |
|------|-------------|
| `--proxy <URL>` | HTTP/SOCKS5 proxy |
| `--user-agent <UA>` | Custom User-Agent string |
| `--stealth` | Enable anti-detection mode |

### Claude Desktop config

```json
{
  "mcpServers": {
    "obscura": {
      "command": "obscura",
      "args": ["mcp"]
    }
  }
}
```

### Tools

| Tool | Description |
|------|-------------|
| `browser_navigate` | Navigate to a URL (`url`, optional `waitUntil`: `load` / `domcontentloaded` / `networkidle0`) |
| `browser_snapshot` | Return the current page URL, title, and body text |
| `browser_click` | Click an element by CSS selector |
| `browser_fill` | Set an input value (triggers `input` + `change` events) |
| `browser_type` | Append text to an input |
| `browser_press_key` | Dispatch a keyboard event (`key`, optional `selector`) |
| `browser_select_option` | Select an `<option>` by value or text |
| `browser_evaluate` | Evaluate a JavaScript expression and return the result |
| `browser_wait_for` | Wait for a CSS selector to appear (`selector`, optional `timeout` in seconds) |
| `browser_network_requests` | List network requests made by the current page |
| `browser_console_messages` | Return console messages logged by the page |
| `browser_close` | Close the page and reset browser state |

## Browser extensions (WebExtensions)

Obscura ships an experimental host for WebExtensions (`obscura-ext`
crate). Load an unpacked directory, `.zip`, `.xpi` (Firefox), or
`.crx` (Chrome) with the global `--extension` flag and Obscura
emulates enough of the `chrome.*` / `browser.*` API for the
extension's background scripts and content scripts to run inside the
page's V8 realm. The full design doc lives at
[`docs/extensions.md`](docs/extensions.md).

The integration is aimed at MV2 content-script extensions that
mutate the DOM (read JSON from `<script>` tags, reshape rendered
content, restyle the page, etc.). MV3 extensions load too, but the
service-worker lifecycle is not emulated — the background runs as a
persistent script in the page realm.

### Quick start

```bash
# Use any unpacked extension, or a packed .zip / .xpi / .crx.
obscura --extension ./my-extension.xpi \
        --stealth \
        fetch https://example.com/article \
        --dump text --wait-until domcontentloaded
```

`--wait-until domcontentloaded` is recommended with extensions:
content scripts mutate the DOM immediately, and skipping the page-
script phase avoids hydration crashes that would otherwise truncate
the extracted text on JS-heavy SPAs.

### What's supported

| Surface | Notes |
|---|---|
| Manifest V2 (Firefox) | Reference target; `background.scripts` runs as a single concatenated chunk in the page realm |
| Manifest V3 (Chrome) | Loads; service-worker lifecycle not emulated (background runs as a persistent script) |
| `chrome.runtime` | `getManifest`, `getURL`, `getPlatformInfo`, `id`, `lastError`, `sendMessage`, `onMessage`, `onInstalled`, `onStartup`, `onConnect`, `openOptionsPage` |
| `chrome.storage.local` / `.sync` / `.session` | In-memory per page; not persisted across navigations |
| `chrome.tabs` | `query`, `get`, `getCurrent`, `sendMessage`, `executeScript`, `update`, `create`, `reload`, `onUpdated`, `onActivated` |
| `chrome.scripting.executeScript` | Loads bundle files into the page realm |
| `chrome.permissions` | `contains` / `request` / `remove` auto-grant the manifest's host_permissions |
| `chrome.cookies` | Stub (returns empty); the underlying `CookieJar` is wired to navigation, not extensions |
| `chrome.webRequest.*` listeners | Register without throwing; do **not** synchronously intercept yet |
| `chrome.declarativeNetRequest` | Rules accepted, no enforcement yet |
| `chrome.action` / `chrome.browserAction` | No-op (no UI) |
| `chrome.management.getSelf` | Returns the loaded manifest's metadata |
| `chrome.offscreen` | No-op (background already has DOM access in our model) |
| `chrome.i18n.getMessage` | Returns empty string |

APIs not on this list are present as no-op stubs so the extension's
startup code doesn't throw — but won't do anything.

### Architecture

Everything (the `chrome` shim, background scripts, content scripts)
runs in **the same V8 realm as the page**, injected as a preload
script before page JavaScript executes. There's no second isolate
for the "background world" and no `tabs` model for multi-page
extensions — Obscura's one-Page-per-context model maps to a single
synthetic tab.

`setTimeout` calls made by the extension are intercepted and queued;
the queue is drained synchronously at the end of the preload so the
extension's DOM mutations land before Obscura's text/HTML extractor
walks the page. This trades the original delay-based retry semantics
for "run everything in flight before returning" — a tradeoff that
works for snapshot-style fetches and `--wait-until domcontentloaded`.

See [`docs/extensions.md`](docs/extensions.md) for the full design
write-up, the per-API shim status, and the diagnostic story when an
extension stops working.

### What's *not* supported (yet)

- `webRequest` blocking listeners do not intercept real traffic — the
  Rust-side `RequestInterceptor` exists in `obscura-net` but isn't
  wired to the JS listener registry yet. Sites that need
  network-level blocking fall back to whatever the content script
  can do post-load.
- `declarativeNetRequest` rules are accepted but not enforced.
- `chrome.storage.local` is in-memory per page; multi-navigation
  flows lose state.
- The popup/options page UI is never rendered. Settings that depend
  on user interaction need to be set programmatically (edit the
  bundle's data tables ahead of time, or pre-seed storage at
  startup).
- One extension at a time; passing `--extension` more than once
  warns and uses the first.

### Loading a `.crx`

Chrome packages (`.crx`) carry a small header (public key + signature
for CRX2, or a serialised protobuf header for CRX3) before the zip
body. Obscura's loader recognises both versions and strips the
header automatically — no need to manually convert to `.zip`.

### Loading an unpacked source tree

If you've cloned the extension's source repository you can point
`--extension` at the unpacked directory:

```bash
obscura --extension ./my-extension-dir fetch <url> --dump text
```

The loader looks for `manifest.json` at the root.

### Diagnostics

Bring-up traces and failures surface at INFO / ERROR through the
`obscura::console` target:

```bash
RUST_LOG=obscura::console=info,obscura_ext=info \
  obscura --extension ./ext fetch <url> --stealth --dump text 2>&1 \
  | grep obscura-ext
```

Look for `obscura-ext: preload starting`, `obscura-ext: drained N
timers`, and any `obscura-ext: ... failed:` lines. The console
subtarget also surfaces `console.log` / `console.error` from the
extension's JS.

## License

Apache 2.0

---
