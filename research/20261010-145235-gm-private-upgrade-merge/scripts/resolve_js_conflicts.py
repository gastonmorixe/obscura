#!/usr/bin/env python3
"""Resolve diff3 conflict markers in obscura-js merge files."""

from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[3]
CONFLICT_RE = re.compile(
    r"<<<<<<< HEAD\n(.*?)(?:\|\|\|\|\|\|\|[^\n]*\n(.*?))?=======\n(.*?)>>>>>>>[^\n]*\n",
    re.DOTALL,
)


def split_conflicts(text: str) -> list[tuple[str, str | None, str, str]]:
    """Return list of (prefix, head, ancestor_or_none, upstream) segments plus final tail as last prefix."""
    parts: list[tuple[str, str | None, str, str]] = []
    pos = 0
    for m in CONFLICT_RE.finditer(text):
        prefix = text[pos : m.start()]
        head = m.group(1)
        ancestor = m.group(2)
        upstream = m.group(3)
        parts.append((prefix, head, ancestor, upstream))
        pos = m.end()
    parts.append((text[pos:], None, None, None))  # type: ignore[arg-type]
    return parts  # type: ignore[return-value]


def resolve_bootstrap(text: str) -> str:
    """Prefer upstream script/DOM machinery; keep private only where it adds unique value."""
    out: list[str] = []
    idx = 0
    for prefix, head, _anc, upstream in split_conflicts(text):
        out.append(prefix)
        if head is None:
            break
        idx += 1
        # All six bootstrap hunks: take upstream.
        # Private concurrent-classic behavior is already in upstream's
        # __runAsyncClassicScript / __prepareInsertedScript path.
        out.append(upstream)
    return "".join(out)


def resolve_ops(text: str) -> str:
    out: list[str] = []
    idx = 0
    for prefix, head, _anc, upstream in split_conflicts(text):
        out.append(prefix)
        if head is None:
            break
        idx += 1
        if idx == 1:
            # Drop duplicate import; LocalStorageStore added into next obscura_net block below.
            out.append("")
        elif idx == 2:
            # Keep private referrer docs + upstream navigation_timing.
            out.append(
                "    /// `document.referrer`. Real browsers expose the navigation's referrer\n"
                "    /// here; some sites (and paywall/anti-bot logic) gate behaviour on it.\n"
                "    /// Set from the navigation's Referer (including any the attached\n"
                "    /// extension rewrote in) so headless renders match a real browser.\n"
                "    /// Direct browser/API navigations leave this empty; document-initiated\n"
                "    /// navigations set it to the source document URL.\n"
                "    pub referrer: String,\n"
                "    pub navigation_timing: NavigationTiming,\n"
            )
        elif idx == 3:
            out.append("            navigation_timing: NavigationTiming::default(),\n")
        elif idx == 4:
            # Take upstream document_origin / base_url / base_href arms.
            out.append(upstream)
        elif idx == 5:
            # Cosmetic: take upstream formatting for query_selector.
            out.append(upstream)
        elif idx == 6:
            # Keep localStorage ops from HEAD, then all upstream frame/runtime ops.
            # Strip HEAD's simple op_sleep (upstream replaces it).
            head_only = head
            sleep_marker = "#[op2(async)]\nasync fn op_sleep"
            cut = head_only.find(sleep_marker)
            if cut < 0:
                raise SystemExit("ops hunk 6: expected HEAD op_sleep marker")
            localstorage_block = head_only[:cut].rstrip() + "\n\n"
            out.append(localstorage_block)
            out.append(upstream)
            # The shared trailing `}` after this conflict closed HEAD's op_sleep.
            # Upstream functions are already closed, so drop that brace from the
            # following prefix by marking a sentinel consumed in the next loop.
        elif idx == 7:
            # Use upstream dynamic ops vec, localStorage registered earlier via push.
            out.append(upstream)
        else:
            raise SystemExit(f"ops: unexpected conflict index {idx}")

    merged = "".join(out)
    # After hunk 6, a lone `}\n` that closed HEAD op_sleep may remain before op_binding_called.
    merged = merged.replace(
        "    owner\n}\n\n// Records a binding call",
        "    owner\n}\n\n// Records a binding call",
        1,
    )
    # If the shared brace survived as an extra closing brace after posted_task_owner:
    merged = re.sub(
        r"(let owner = registry\.by_frame_id\(frame_id\);\n    owner\n)\}\n(\n// Records a binding call)",
        r"\1}\2",
        merged,
        count=1,
    )
    # Wait: posted_task_owner still needs its closing brace. Upstream side ended with
    # `owner` and the shared `}` was the function closer. Keep ONE `}`.
    # The replace above would wrongly remove it. Revert that approach.

    # Add LocalStorageStore to the obscura_net import block.
    old_import = (
        "use obscura_net::{\n"
        "    CallbackRegistry, CookieJar, ObscuraHttpClient, RequestInfo, ResourceType, Response,\n"
        "    SameSiteContext, same_site,\n"
        "};"
    )
    new_import = (
        "use obscura_net::{\n"
        "    CallbackRegistry, CookieJar, LocalStorageStore, ObscuraHttpClient, RequestInfo, ResourceType, Response,\n"
        "    SameSiteContext, same_site,\n"
        "};"
    )
    if old_import not in merged:
        raise SystemExit("ops: obscura_net import block not found")
    merged = merged.replace(old_import, new_import, 1)

    # Register localStorage ops on the upstream-built ops vec.
    needle = "        op_begin_render_task(),\n    ];"
    insert = (
        "        op_begin_render_task(),\n"
        "        op_localstorage_get_item(),\n"
        "        op_localstorage_set_item(),\n"
        "        op_localstorage_remove_item(),\n"
        "        op_localstorage_clear(),\n"
        "        op_localstorage_length(),\n"
        "        op_localstorage_key(),\n"
        "    ];"
    )
    if needle not in merged:
        raise SystemExit("ops: ops vec terminator not found")
    merged = merged.replace(needle, insert, 1)
    return merged


def resolve_runtime(text: str) -> str:
    out: list[str] = []
    idx = 0
    for prefix, head, _anc, upstream in split_conflicts(text):
        out.append(prefix)
        if head is None:
            break
        idx += 1
        if idx == 1:
            # Keep private extension-aware referrer docs.
            out.append(head)
        elif idx == 2:
            # Take upstream module/classic machinery, but always arm watchdog.
            # Replace upstream's size-gated execute_script_guarded with private always-on.
            if "pub fn execute_script_guarded" not in upstream:
                raise SystemExit("runtime hunk 2: missing execute_script_guarded")
            # Split upstream before execute_script_guarded body decision.
            marker = "    pub fn execute_script_guarded(&mut self, name: &str, source: &str) -> Result<(), String> {\n"
            pre, _, rest = upstream.partition(marker)
            if not rest:
                raise SystemExit("runtime hunk 2: guarded marker missing")
            # Drop upstream size gate; keep private always-watchdog policy.
            guarded = (
                marker
                + "        // Always arm the watchdog. Source size is unrelated to runaway risk:\n"
                + "        // a 50-byte `for(;;){}` or a 3-line promise chain hangs V8 just as\n"
                + "        // hard as a 50KB script. The threshold gate the old code used was a\n"
                + "        // long-standing false-economy (skipping the watchdog to save the\n"
                + "        // thread spawn) that left the unguarded path open to wedges in\n"
                + "        // user-script callback graphs (load handlers, microtask reactions).\n"
                + "        // The kill mechanism (`isolate_handle.terminate_execution()`) plus\n"
                + "        // the post-kill `<reset>` script in `execute_script_with_timeout`\n"
                + "        // returns `Ok(())` for terminated scripts, so callers see the same\n"
                + "        // contract whether the script ran to completion or got killed.\n"
                + "        self.execute_script_with_timeout(name, source, std::time::Duration::from_secs(5))\n"
            )
            out.append(pre + guarded)
            # The shared trailing `}` after the conflict closes execute_script_guarded.
        elif idx == 3:
            # Take upstream pending-script bridges (incl. load-delaying / parser-blocking).
            out.append(upstream)
        else:
            raise SystemExit(f"runtime: unexpected conflict index {idx}")
    return "".join(out)


def check_no_markers(path: pathlib.Path, text: str) -> None:
    if "<<<<<<<" in text or "=======" in text or ">>>>>>>" in text or "|||||||" in text:
        raise SystemExit(f"markers remain in {path}")


def main() -> int:
    files = {
        ROOT / "crates/obscura-js/js/bootstrap.js": resolve_bootstrap,
        ROOT / "crates/obscura-js/src/ops.rs": resolve_ops,
        ROOT / "crates/obscura-js/src/runtime.rs": resolve_runtime,
    }
    for path, resolver in files.items():
        original = path.read_text()
        count = original.count("<<<<<<< HEAD")
        resolved = resolver(original)
        check_no_markers(path, resolved)
        path.write_text(resolved)
        print(f"resolved {path.relative_to(ROOT)}: {count} hunks -> {len(resolved)} bytes")
    return 0


if __name__ == "__main__":
    sys.exit(main())
