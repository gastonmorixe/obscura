# Upstream sync and the Chrome 156 stealth identity

Report date: 2026-10-31
Branch: `gm/private-upgrade-20261010`
Commits: `2fd8e6c` (merge), `b095b5b`, `423ca1d`, `d0a1a12`, `e48cb3f`

This report records what changed in this pass, why, and the evidence behind
each claim. It is a working record, not a release note.

## 1. Summary

Two bodies of work landed together.

1. The private branch was 201 commits behind `upstream/main`. It is now synced
   to `v0.2.5-109-g800b5ba`, with every fork-only feature still in place.
2. The stealth identity moved from Chrome 148 to Chrome 156. The version bump
   is not cosmetic: it is what gets past PerimeterX on Bloomberg.

Along the way, independent review found five real defects. All are fixed, each
with a test where a test is possible.

## 2. Upstream sync

### 2.1 Shape

The merge was done in a throwaway worktree first. The live tree was
fast-forwarded only after the merged result compiled and passed the full suite.

Base `73299d5`, upstream `800b5ba`, 10 conflicted files:

| file | conflicts | resolution |
| --- | --- | --- |
| `crates/obscura-browser/src/context.rs` | 6 | upstream base, fork extension field re-applied by hand |
| `crates/obscura-js/js/bootstrap.js` | 1 | upstream, then fork identity re-applied |
| `crates/obscura-js/src/ops.rs` | 3 | upstream storage, dead fork ops deleted |
| `crates/obscura-js/src/runtime.rs` | 1 | upstream |
| `crates/obscura-browser/src/page.rs` | 1 | upstream |
| `crates/obscura-cdp/src/domains/accessibility.rs` | 1 | upstream (module moved) |
| `crates/obscura-cli/src/main.rs` | 2 | upstream `save_storage()` |
| `crates/obscura-mcp/src/lib.rs` | 1 | upstream comment, `save_storage()` |
| `.github/workflows/ci.yml` | 1 | kept both steps |
| `Cargo.lock` | 9 | regenerated from manifests |

### 2.2 The one deliberate drop

The fork had `obscura_net::LocalStorageStore`, one JSON file holding every
origin. Upstream now ships `obscura_js::ops::OriginStorage`, which is a
strict superset:

- one file per origin under `{storage_dir}/localStorage/`
- atomic writes through a temp file
- byte and origin-count budgets
- `sessionStorage` scoped to the tab
- IndexedDB
- `SecurityError` on an opaque origin

Keeping both would mean two sources of truth. The fork store was dropped.

### 2.3 Fork-only features verified still live

| feature | evidence |
| --- | --- |
| `obscura-ext` WebExtension host | all 8 files present, `git diff` against the fork snapshot is empty |
| `BrowserContext::with_extension` | `context.rs`, wired from the CLI |
| extension request-header prepass | 4 tests in `extension_request_headers.rs` |
| `document.referrer` mirroring | behaviour test: document-initiated nav sets it, direct nav leaves it empty |
| stealth identity | wire capture and JS probe agree |
| `--extension`, `--storage-dir`, `--user-agent` | present in help output |
| public `Page` interception API | `add_preload_script`, `enable_interception`, `on_request`, `on_response` |

## 3. The stealth identity: 148 to 156

### 3.1 Why the bump was not cosmetic

PerimeterX (HUMAN) serves a challenge page titled
`Bloomberg - Are you a robot?` before the article. The challenge was not caused
by the extension, cookies, or TLS.

Evidence, in the order it was gathered:

1. Real Chrome 156 on this machine loaded the article in a **fresh incognito
   context with zero cookies**. Cookies were never the gate.
2. Plain `node fetch` with a Chrome 156 UA returned 428 KB of article. TLS was
   never the gate.
3. `curl` with two **identical header sets that differed only in the version
   string**:

| UA major | result |
| --- | --- |
| 154 | `Bloomberg - Are you a robot?` |
| 155 | article |
| 156 | article |
| 157 | article |
| 160 | article |

That is a one-byte difference in one header, and it flips the response. Our
engine advertised 154.

### 3.2 What changed

Every identity surface moved to 156:

- `STEALTH_USER_AGENT` and `STEALTH_SEC_CH_UA` in `wreq_client.rs`
- the non-stealth client defaults and GREASE fallback in `client.rs`
- the profile table in `profiles.rs`
- `navigator.userAgent`, `appVersion`, and the `_chromeMajor` default in
  `bootstrap.js`
- the scripted-fetch UA fallback in `ops.rs`
- CDP `Browser.getVersion` and `/json/version`
- `stealth_transport.rs` expectations

The 2-brand GREASE rule for major >= 148 is unchanged. At 156 the arithmetic
is `156 % 11 = 2` (space), `157 % 11 = 3` (`(`), `156 % 3 = 0` (`8`), which
gives `"Not A(Brand";v="8", "Chromium";v="156"`. Real Chrome 156 on this
machine reports exactly that.

### 3.3 The TLS profile stays at 154

`wreq-util` ships profiles up to `Profile::Chrome154`. Main caps there; no 156
profile exists. Advertising 156 while using the 154 TLS profile is safe: the
ClientHello carries no Chrome version, so the two cannot contradict each other
on the wire. This is deliberate and commented in `wreq_client.rs`.

### 3.4 The duplicate user-agent

The stealth transport used to set the emulation profile UA and the resolved UA
separately, which put two `user-agent` headers on the wire. Precedence is now a
single chain:

1. per-request header
2. client extra headers
3. `--user-agent`
4. the stealth identity

One resolution decides the value, and the header loop skips `user-agent` so it
cannot add another. `stealth_user_agent_override_has_one_wire_value_for_navigation_and_script_fetch`
pins this for the navigation path and the scripted-fetch path.

Trap encountered and backed out of: sending the stealth UA explicitly on every
request, alongside the profile, made the transport time out. The default
identity must stay with the emulation profile.

## 4. Defects found by independent review

Each was found by a separate reviewer with a different point of view, then
fixed and pinned.

### 4.1 AX node ids regressed (major)

Upstream assigns AX ids from an ordinal counter. The fork used DOM-derived ids.
An insertion earlier in the document renumbers every AX node, which breaks
client caching. Synthetic bounds were also dropped entirely, though
`docs/persistent-fetch-worker-protocol.md` still promises both.

Fixed in `crates/obscura-browser/src/accessibility.rs`: ids are
`format!("ax-{}", dom_id.raw())` again and the `bounds` block is back, marked
`"synthetic": true`. Pinned by
`ax_node_ids_derive_from_dom_ids_and_bounds_are_marked_synthetic`.

### 4.2 Silent storage loss on upgrade (major)

Upstream reads only `{storage_dir}/localStorage/*.json`. A profile written by
the fork has one `{storage_dir}/localstorage.json`. No migration meant an
upgrade would silently start with empty `localStorage` while cookies survived.

Fixed with `load_legacy_web_storage`, which adopts the old file once and leaves
it in place for a downgrade. The first attempt failed a real test because a
missing per-origin directory took an early `return`. Pinned by
`legacy_single_file_localstorage_is_migrated_on_upgrade`.

### 4.3 Extension prepass read a throwaway store (major)

The prepass realm that runs extension scripts before page JS never bound the
context Web Storage. It kept the private store that `ObscuraState` allocates by
default and dropped it with the realm. An extension that reads `localStorage`
to shape a request header saw empty data with no error.

Fixed in `page.rs`: the prepass now binds `local_storage`, `indexed_db`, and a
fresh `session_storage`.

### 4.4 Duplicate user-agent (major)

Covered in section 3.4.

### 4.5 Stale call sites after the merge

`fetch_worker.rs` referenced the AX builder at its old path, and three call
sites used `save_session()`, which no longer exists. Fixed by pointing at
`obscura_browser::accessibility::build_ax_nodes_with_styles` and
`save_storage()`.

## 5. What the extension host still does not do

This is a real gap, found during the Bloomberg investigation and **not fixed**.

`onBeforeRequest`, `onHeadersReceived`, and declarativeNetRequest blocking are
recorded but never executed for subresources. `runtime.rs` hardcodes
`"main_frame"`, and `listeners_for` / `dnr_blocks` have no call sites outside
their own definitions.

The consequence: an extension rule that blocks a stylesheet, a script, or a
font does nothing, because only the top-level document goes through the header
chain. Fixing it needs a persistent extension realm per page, which costs
memory per page. It is the next meaningful piece of work in `obscura-ext`.

For the Bloomberg case this gap was moot: the block rule BPC defines targets a
CSS file behind a document that never loaded.

## 6. Known limitations after this pass

- **Hydration.** Bloomberg delivers its full body in `__NEXT_DATA__` (27
  paragraphs, confirmed). Our engine's DOM reaches about 12 `<p>` elements
  against real Chrome's 67. `--dump text` shows the lede and then page
  scaffolding. The content is in the delivered HTML; the DOM does not expand
  it. This is a JS runtime gap, separate from the bot wall.
- **`--wait` on Bloomberg hangs.** Statsig retries never settle, so the
  deadline fires. Use `--wait-until load`.
- **Timezone default.** The CLI falls back to `Europe/Berlin` when `TZ` is
  unset, while advertising a US-locale identity. A cross-surface mismatch a
  fingerprinter can see. Set `OBSCURA_TIMEZONE` to match the exit IP.
- **Non-stealth runs still advertise Chrome 156** and pass the same gate, but
  they do not carry the stealth transport's header set.
- **Dead code.** `obscura_net::localstorage` is gone from use but its module
  doc and a `pub use` may remain. `docs/persistence.md` still describes the old
  single-file format.

## 7. Verification

| check | result |
| --- | --- |
| `cargo nextest run --release --features render,stealth --no-fail-fast` | 2556 passed, 5 skipped |
| build `--features render,stealth` | exit 0 |
| build `--features render` | exit 0 |
| build `--no-default-features` | exit 0 |
| build `--no-default-features --features stealth` | exit 0 |
| wire identity | one `user-agent`, `sec-ch-ua: "Not A(Brand";v="99", "Chromium";v="156"` |
| JS identity | `navigator.userAgent` 156, brands 2, platform macOS |
| Bloomberg, stealth | 438 KB article, correct title |
| Bloomberg, stealth + BPC extension | 438 KB article, 27 paragraphs extractable |
| fail-first proof on the prepass fix | test fails without `run_page_init`, passes with it |

Not run: the 33-stage obstacle course in `obscura-benchmark`. That repository is
not present on this machine.
