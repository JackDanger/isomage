---
session: "coverage-lib-ntfs"
timestamp: "2026-05-13T20:55:00Z"
model: claude-sonnet-4-6
---

## Human

Make sure all of our work is in PRs and/or merged and that there's fantastic tests with 100% coverage over everything. Release new versions as appropriate.

## Assistant

### Key decisions

**lib.rs extract path coverage**: Added three tests that hit previously uncovered production branches:
- `safe_join_detects_here_outside_root`: calls `safe_join` with `here` outside `root`, exercising the path-escape error at lines 361–366
- `extract_node_errors_when_no_file_location`: calls `extract_node` with a `TreeNode::new_file` (no location), exercising the "location not available" error in `extract_file_at` at line 394
- `cat_node_swallows_broken_pipe_after_partial_write`: exercises the `Ok(take)` path in `BrokenPipeAfter` with a non-zero budget

Also simplified `BrokenPipeAfter::write` by removing a dead `if take == 0` branch that was unreachable from `cat_node` (which never calls `write` with 0-byte slices).

**ntfs.rs coverage**: Added Error Display and source tests for all five Error variants (TooShort, BadMagic, BadClusterSize, TooDeep, Io), and four `parse_boot_sector` error path tests:
- `parse_boot_sector_too_short`: data shorter than 84 bytes → TooShort
- `parse_boot_sector_bad_magic`: wrong OEM ID → BadMagic
- `parse_boot_sector_bad_sector_size`: bytes_per_sector = 0 → BadClusterSize
- `parse_boot_sector_zero_sectors_per_cluster`: sectors_per_cluster = 0 → BadClusterSize

These covered lines 82–90, 93–99, 102–105, 120, 125, 133, 136 in ntfs.rs.
