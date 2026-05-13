---
pr: 17
slug: qcow2-copilot-fixes
date: 2026-05-13
---

# PR #17 QCOW2 reader — Copilot review fixes

## Issues addressed

### Module doc "68-byte" should be "72-byte" (low)

The module-level comment said "the 68-byte common header is parsed for both"
versions, but `HEADER_SIZE` is 72 and the header layout table lists bytes 0..72
(ending with `u64 snapshots_offset` at offset 64, which takes the header to byte
72). Changed "68-byte" to "72-byte".

### Unused `Header` struct fields (low)

`Header` stored `version`, `cluster_bits`, and `encryption_method` fields that
were only used for validation inside `read_header` and discarded with
`let _ = hdr.field` immediately after. Changed `Header` to only carry
`disk_size` (the only field used by the caller). This removes the dead-code
suppressions and makes the struct's contract self-documenting.
