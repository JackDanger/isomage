---
pr: 22
slug: dmg-copilot-fixes
date: 2026-05-13
---

# PR #22 DMG reader — Copilot review fixes

## Issues addressed

### hdiutil tests panic on Linux with ISOMAGE_REQUIRE_TOOLS=1 (critical)
`require_or_skip()` panics when `ISOMAGE_REQUIRE_TOOLS=1` is set and the
tool is missing. On Linux, `hdiutil` is never available. The strict round-
trip Ubuntu CI job would therefore panic on every hidiutil-dependent test.

Fixed by adding `#[cfg(target_os = "macos")]` to `make_hdiutil_dmg()` and
all four tests that call it (dmg_detect, dmg_detect_restores_cursor,
dmg_koly_fields, dmg_partition_names). These tests simply don't exist on
Linux; the hand-crafted-koly tests still run on all platforms.

### koly layout documentation contradicted implementation (low)
Module doc listed `xml_offset` at offset 212 and `xml_length` at 220, but
the parser reads them at 216 and 224 (correct, verified against hdiutil
output). Fixed the layout table to show the 4-byte reserved field at 212
before xml_offset at 216.
