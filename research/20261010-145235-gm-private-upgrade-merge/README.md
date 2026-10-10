---
title: Private upgrade merge research pack
created_at: "2026-10-10T14:52:53.060064000-0400"
updated_at: "2026-10-10T14:52:53.060064000-0400"
session_id: ae2d0a5f-2b47-4128-a2b5-47d71203a492
host_info:
  hostname: redacted
  user: redacted
  os: "macOS 27.0.1 (26A434)"
  kernel: "27.0.0"
  arch: arm64
  serial: redacted
tags: [obscura, merge, gm-private, upstream-sync, stealth]
taillog:
  - "2026-10-10T14:52:53.060064000-0400 | Organized upgrade-merge notes into timestamped pack; scrubbed home paths and owner names"
---

# Private upgrade merge research pack

Notes from merging `upstream/main` into `gm/private-upgrade-20261010` while keeping private-line features.

## Layout

- `notes/`: inventories, conflict resolve logs, post-merge audit, render-fail compare
- `scripts/`: one-shot merge helper used during conflict resolution (repo-relative paths only)

## Key findings

1. Private must-keep set: WebExtension, localStorage/`--storage-dir`, Chrome 148 stealth, fetch-worker/AX dumps, watchdog, dist CI, MCP pub RPC types.
2. Conflict files resolved with private behavior preserved. See `notes/resolve-*.md`.
3. Stealth release build compiled clean. Private-fix audit: 8/8 PASS (`notes/private-fixes-audit.md`).
4. Merge-related nextest breaks fixed (stealth UA test, CDP `session_persistence` registry, duplicate AX `mod tests`, lifecycle EventEnd on terminate).
5. `obscura-render` 29 fails match bare `upstream/main` on this host. Deferred as baseline, not merge damage (`notes/render-fail-compare.md`).

## Privacy scrub

- No home-directory paths.
- No host serial, hostname, or local username.
- Dist repo owner redacted to `<owner>/obscura-dist`.
- Scratch nextest dumps with absolute cargo/home paths were deleted, not committed.

## Related commits

- Merge: `5488f3d` Merge upstream/main into gm/private-upgrade-20261010
- Follow-up: `29f6af7` fix(merge): keep private stealth and termination contracts after upstream sync
