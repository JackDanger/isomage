---
pr: 20
slug: wim-copilot-fixes
date: 2026-05-13
---

# PR #20 WIM reader — Copilot review fixes

## Issues addressed

### original_size unbounded allocation (critical)
`xml_res.original_size as usize` was cast and used directly to allocate a
`Vec` before any bounds check. A malformed WIM header could drive OOM or
truncation on 32-bit targets. Fixed by:
- Validating `original_size <= 64 MiB` (max cap; real WIM XML is << 1 MiB)
- Validating `offset + original_size <= file_size`
- Returning `Error::TooShort` on either violation

### image_count from header driving excessive allocation (medium)
When XML parsing yields no `<IMAGE>` entries, the fallback loop used
`header.image_count` directly. A corrupt header could set this to 4 billion.
Capped at 4096 synthetic image nodes.
