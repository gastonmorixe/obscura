# Extension Host BPC Paywall Report

Branch: `gm/private-upgrade-20261010`. Work is uncommitted. Date: 2026-11-01.

## Goal

Make the Bypass Paywalls Clean (BPC) extension work under the Obscura
extension host so paywalled pages return full article text. No site-specific
hacks in runtime code. Tests only.

## Dump format matrix

`fetch --dump` formats compared on paywalled targets:

| Format | Notes |
| --- | --- |
| html | Raw DOM serialisation. Largest output. Keeps paywall shells. |
| orig | Original server bytes. Baseline for server-gated checks. |
| text | Readable-text extraction plus whitespace collapse (this work). |
| markdown | HTML to markdown plus whitespace collapse (this work). |
| links | URL list. Useful to confirm article vs challenge page. |

Reference sizes from real Chrome via CDP (same machine, same network):

| Page | Chars | Paragraphs |
| --- | --- | --- |
| WSJ article | 10300 | 67 |
| Bloomberg article | 10224 | - |

## Root causes

1. **Bootstrap SyntaxError, truncated starToRegex line.** The injected
   `chrome_shim.js` bootstrap had a truncated line in the `starToRegex`
   helper. V8 rejected the whole preload script. No extension API existed
   at content-script time. Fix: repair the line, add a syntax gate.
2. **Injection timing on empty DOM.** The preload ran before any document
   existed. Content scripts that query the DOM at load found nothing and
   never re-ran. Fix: split preload into bootstrap (API shell, runs early)
   plus idle-time injection (DOM present).
3. **DNR block and allow missing.** The host parsed only `modifyHeaders`
   rules. BPC `block` rules for paywall scripts were dropped, and `allow`
   overrides did not exist. Fix: parse and match `block` / `allow` with
   initiator scope. `allow` outranks `block` at equal priority per spec.
4. **Request hooks not reaching the network layer.** `onBeforeRequest` and
   header hooks were recorded but never fired for subresources. Only the
   top-level document ran through the header chain. Fix: wire hooks into
   fetch worker path (see `docs/extensions.md`).
5. **Text and markdown dumps full of blank lines.** Paywall shells leave
   empty blocks. 3+ newline runs buried the article. Fix: whitespace-only
   collapse, max 1 blank line, trailing spaces stripped. No content removed.

## Fixes

- `crates/obscura-ext/js/chrome_shim.js`: bootstrap repair plus syntax gate.
- `crates/obscura-ext/src/runtime.rs`: split preload bootstrap plus idle.
- `crates/obscura-ext/src/state.rs`: extension state for late injection.
- `crates/obscura-ext/src/dnr.rs`: `BlockRule` with block and allow plus
  initiator scope. `redirect` and `upgradeScheme` stay out of scope.
- `crates/obscura-cli/src/main.rs`: `collapse_dump_text_whitespace` and
  `collapse_dump_markdown_whitespace`.
- `crates/obscura-cli/src/fetch_worker.rs`, `worker.rs`: hook wiring.
- `crates/obscura-browser/src/page.rs`: page-side injection support.
- Tests: `extension_request_headers.rs`, `extension_late_injection.rs`.

## Verification

| Case | Result |
| --- | --- |
| FBI Bloomberg, with extension | 3949 chars, full body, no syntax error |
| Midterms Bloomberg, with extension | 9913 chars, full body |
| WSJ, with extension | 2142 chars, lede only, server-gated |

Chrome CDP reference confirms the gap is server-side for WSJ, not a host
bug: real Chrome returns 10300 chars and 67 paragraphs where Obscura gets
the lede.

## Known limits

- **WSJ encrypted payload.** Full text arrives encrypted and is decrypted
  client-side after entitlement. Server gates on session. No host fix.
- **RSC fetch.** Next.js App Router flight data fetches bypass the hooked
  paths. Out of scope for this work.
- **Reuters DataDome.** Bot challenge blocks before content. Separate
  stealth problem.

## Rule kept

No hostname-specific layout, style, or resource behavior in runtime. All
site-specific assertions live in tests.
