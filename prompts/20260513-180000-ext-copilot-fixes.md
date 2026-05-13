---
pr: 13
slug: ext-copilot-fixes
date: 2026-05-13
---

# PR #13 ext reader — Copilot review fixes

## Issues addressed

### log_block_size shift panic (critical)
`1024u64 << log_block_size` would panic if a malformed superblock set
`log_block_size >= 64`. Added explicit rejection of `log_block_size > 6`
(ext max block size is 64 KiB = 1024 << 6).

### read() → read_exact() for superblock (high)
`file.read(&mut sb)` could return a short read without EOF, causing
nondeterministic `TooShort` results. Switched to `read_exact` with
`UnexpectedEof` → `Error::TooShort` mapping. Since we always have 264
bytes, the `n >= 238` guard was removed.

### Unwritten extents producing file_location (medium)
`parse_leaf_extents` already masked bit 15 of `ee_len` but discarded
the flag. Added `unwritten: bool` to `Extent` and `single_run_location`
now skips returning a location for unwritten extents (preallocated but
not yet written — reading would yield stale data).

### Symlink fast-symlink bogus file_location (medium)
Symlinks may store the target path in `i_block` directly (fast symlinks).
The old code called `single_run_location` for symlinks, which would treat
those path bytes as a block number and produce a bogus offset. Symlinks
now always get `file_location = None`.
