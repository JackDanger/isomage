---
pr: 12
slug: fat-copilot-fixes
date: 2026-05-13
---

# PR #12 FAT reader — Copilot review fixes

## Issues addressed

### cluster_chain doesn't validate cluster range or bad-cluster markers (medium)

`cluster_chain` only broke out of the loop on `cluster < 2` or EOC. A
corrupt image could supply cluster numbers above `total_clusters + 1` (valid
data range is 2..=total_clusters+1), or the reserved bad-cluster value
(0xFF7/0xFFF7/0x0FFFFFF7 for FAT12/16/32), causing the walker to read far
into the FAT or loop for a very long time before hitting the length limit.

Added `is_bad_cluster()` parallel to `is_eoc()`. Updated the loop to break
when `cluster > max_valid` or `is_bad_cluster`. Changed the cycle guard from
`total_clusters + 2` comparisons to `chain.len() > total_clusters` (tighter
and more correct for the round-trip case).

### file_location set without verifying chain covers the full file (medium)

A truncated or corrupt FAT chain that is contiguous for the first few
clusters would pass `is_contiguous()` and produce a `file_location`, causing
`cat_node` to read past the file's allocated area. Added:

```rust
let required_clusters = (entry.file_size as u64).div_ceil(ctx.bytes_per_cluster) as usize;
// only set file_location if chain covers the whole file
chain.len() >= required_clusters
```

### Double calculate_directory_size traversal (low)

`build_tree` called `dir_node.calculate_directory_size()` for every
directory (which is itself recursive), and then `detect_and_parse` called
it again on the root, re-traversing the entire tree a second time. Removed
the per-directory call; the single root-level call already descends the
whole tree.

### LFN unit test has wrong string and weak assertion (low)

The comment said "LongFileName.txt" (16 chars) but the code used
"LongFileNam.txt" (15 chars) — neither fits cleanly in one 13-char LFN
entry. The assertion only checked `!name.starts_with("LONGFI")`, which
passes even if LFN reassembly is partially broken.

Changed the test name to "LongFile.txt" (12 chars), which fits in a single
LFN entry with its null terminator. Changed the assertion to `assert_eq!`
against the exact expected string.
