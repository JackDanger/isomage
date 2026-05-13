---
pr: 14
slug: squashfs-copilot-fixes
date: 2026-05-13
---

# PR #14 SquashFS reader — Copilot review fixes

## Issues addressed

### Clippy job missing features (medium)
The clippy CI job used `--all-targets` but no `--features`, so all
feature-gated code (squashfs, ext, mbr, etc.) was excluded from linting.
Added `--features mbr,gpt,raw,ext,squashfs,mmap,simd` to the clippy invocation.

### block_size == 0 divide-by-zero panic (medium)
`block_count_for(file_size, block_size, fragment)` divides by `block_size`.
A corrupt superblock with `block_size = 0` would panic. Added an explicit
check in `Superblock::read` that rejects zero block sizes with `Error::BadMagic`.

### Big-endian SquashFS detection falling through to LE parser (medium)
The superblock reader detected BE magic (0x68737173) and set `little_endian=false`,
but all subsequent parsing used LE helpers. This would silently mismatch every
inode and directory entry on a real BE image. Since SquashFS v4 in practice
is always LE, big-endian images are now rejected with `Error::BadMagic`.
