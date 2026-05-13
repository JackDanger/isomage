---
pr: 18
slug: ntfs-copilot-fixes
date: 2026-05-13
---

# PR #18 NTFS reader — Copilot review fixes

## Issues addressed

### apply_fixup return value ignored (high)
`read_mft_record` called `apply_fixup(&mut buf)` but ignored the bool.
If the USA header is malformed, continuing to parse yields corrupted data.
Now returns `Ok(None)` (skip record) when fixup fails.

### attr_type / attr_length confusion (high)
In `extract_record_info`, the variable reuse `let attr_length = buf[attr_pos..+4]`
then `if attr_length == ATTR_END` was reading the **attribute type** but
naming it `attr_length`. The check was correct but deeply misleading.
Renamed to `attr_type_check` so the two reads are clearly distinguished.

### Non-resident file_location missing volume base offset (high)
`Some(runs[0].start_lcn * cluster_size)` did not include the `base` offset
recorded at the start of `detect_and_parse`. For NTFS embedded in a
partition image, this produces an offset relative to the partition start,
not the image start. Added `volume_base` parameter to `extract_record_info`
and included it in the computed byte offset.

### Sparse runs in runlist producing false file_location (medium)
`decode_runlist` dropped sparse entries (off_size == 0). A sparse+data
runlist with a single data run would produce `runs.len() == 1` and a
`file_location` even though the logical extent is not contiguous on disk.
`decode_runlist` now returns `(Vec<Run>, bool)` where the `bool` is
`had_sparse`. `file_location` is only set when `had_sparse` is false.

### MFT scan stops at first free record (medium)
Free/unused MFT slots return `Ok(None)` from `read_mft_record`, and the
old code broke the walk at the first None after the system record range.
Real volumes have free slots interspersed, so later files were silently
dropped. Removed the early break; the walk now continues until the 1M
guard or EOF.
