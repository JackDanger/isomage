---
pr: 16
slug: vmdk-copilot-fixes
date: 2026-05-13
---

# PR #16 VMDK reader — Copilot review fixes

## Issue addressed

### capacity * SECTOR_SIZE u64 overflow (critical)
`header.capacity * SECTOR_SIZE` could overflow `u64` on a malformed image
with a very large `capacity` field (wraps silently in release, panics in
debug). Changed to `checked_mul`, returning `Error::BadMagic` on overflow.
