---
session: "squashfs-dead-code"
timestamp: "2026-05-14T18:40:00Z"
model: claude-sonnet-4-6
---

## Human

Why do we have code in unreachable states?

## Assistant

### Key decisions

The `Superblock::read` function had dead code in three places:

**Root cause**: `MAGIC_LE` and `MAGIC_BE` are byte-reversals of each other
(`MAGIC_LE.swap_bytes() == MAGIC_BE`). A LE squashfs image stores `MAGIC_LE`
in LE byte order as `[0x68, 0x73, 0x71, 0x73]`; a BE squashfs image stores
`MAGIC_LE` in BE byte order as `[0x73, 0x71, 0x73, 0x68]`. Reading either
with `from_le_bytes` gives `MAGIC_LE` for LE and `MAGIC_BE` for BE — so
the two constants are distinguishable from LE reads alone.

The original code computed both `from_le_bytes` and `from_be_bytes`, then
checked `magic_le == MAGIC_LE` first in an if-else chain. Because any 4
bytes where `from_be_bytes == MAGIC_BE` also satisfy `from_le_bytes == MAGIC_LE`
(same bytes, different interpretation), the `false` branch (`little_endian = false`)
was never reached. The closures `u16_at`/`u32_at`/`u64_at` then carried
dead `else { from_be_bytes(...) }` arms, and the subsequent
`if !little_endian { return Err(BadMagic) }` guard was also dead.

**Fix**:
- Removed `little_endian` flag and the three conditional closures.
- Replaced with two sequential checks: `if magic == MAGIC_BE { reject }` then `if magic != MAGIC_LE { reject }`.
- Simplified closures to unconditional `from_le_bytes`.
- Same fix applied to `detect_inner`: removed the dead `be == MAGIC_BE` branch.
- Updated tests to use `MAGIC_BE.to_le_bytes()` (actual BE on-disk bytes `[0x73, 0x71, 0x73, 0x68]`) rather than `MAGIC_BE.to_be_bytes()` (which was accidentally producing LE squashfs bytes).
- Updated module doc comment to say BE is rejected rather than "detected and tracked".
