---
pr: 23
slug: apfs-copilot-fixes
date: 2026-05-13
---

# PR #23 APFS reader — Copilot review fixes

## Issues addressed

### read() → read_exact() throughout (high)
`Read::read` may return fewer bytes than requested without signaling EOF, causing
false `TooShort` errors or silently using zeroed memory for the unread portion.
Fixed all six call sites in apfs.rs to use `read_exact`, mapping `UnexpectedEof`
to `Error::TooShort` and propagating actual I/O errors.

The `fs_oid` loop now breaks cleanly on `UnexpectedEof` (end of oid array) while
propagating real I/O errors.

The `read_volume_name` function now uses `read_exact(...).ok()?` which returns
`None` on any I/O error (including EOF), preventing partial-read name decoding.

### Unconditionally-skipping hidiutil test (low)
`apfs_detect_hdiutil` called `require_or_skip()` and then always skipped.
On Linux with `ISOMAGE_REQUIRE_TOOLS=1`, `require_or_skip()` panics.
Fixed by:
- Adding `#[cfg(target_os = "macos")]` so the test doesn't exist on Linux
- Adding `#[ignore]` since the test cannot run without root access even on macOS
