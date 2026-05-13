---
pr: 15
slug: vhd-copilot-fixes
date: 2026-05-13
---

# PR #15 VHD reader — Copilot review fixes

## Issues addressed

### parse_fixed: current_size not validated against file_len (high)
`parse_fixed` was called with only `current_size`, without knowing the actual
file length. A malformed footer with `current_size > file_len - FOOTER_SIZE`
would produce a `disk.img` node pointing past EOF. Added `file_len` parameter
and reject when `current_size > file_len - FOOTER_SIZE` (→ `Error::TooShort`).

### parse_dynamic: data_offset not validated (medium)
`data_offset` from the footer was used directly as a seek target without bounds
checking. A corrupt/huge `data_offset` would seek far past EOF and produce a
generic I/O error. Added: if `data_offset > file_len - 1024`, return TooShort.
Also mapped `UnexpectedEof` from `read_exact` to `Error::TooShort`.

### Test comment inaccuracy (low)
`checksum_corrupted_footer_fails` had comment "Corrupt byte 0 (part of the cookie)"
but the code mutated `footer_start + 10` which is the features field, not the
cookie. Removed the misleading comment; kept the inline description accurate.
