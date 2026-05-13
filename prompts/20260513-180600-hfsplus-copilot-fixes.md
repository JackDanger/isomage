---
pr: 19
slug: hfsplus-copilot-fixes
date: 2026-05-13
---

# PR #19 HFS+ reader — Copilot review fixes

## Issues addressed

### read() → read_exact() throughout (high)
All four `r.read(&mut buf)?` calls could return a short read without EOF,
causing false TooShort errors. Replaced with `read_exact`, mapping
`UnexpectedEof` → `Error::TooShort` and propagating real I/O errors.

### 512-byte header node buffer too small for large node_size (high)
The B-tree header node was read into a 512-byte buffer. The offset table
for record 0 was then read from bytes [510..512] of that buffer. If
`node_size > 512`, byte 510 of the 512-byte buffer is NOT the last offset
table entry — the actual table is at `node_size - 2` in the full node.

Per HFS+ spec (TN1150 §2.2), the BTHeaderRec ALWAYS starts at byte 14 of
the header node (immediately after the 14-byte node descriptor). We now
read exactly 120 bytes (14 + 106) and parse the header record from byte 14,
without relying on the offset table. This is correct for any node_size.
