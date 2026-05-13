---
pr: 15
slug: vhd-remaining-fixes
date: 2026-05-13
---

# PR #15 VHD reader — remaining Copilot fixes

## Issues addressed

### Module doc says "footer at byte 0" for Dynamic VHDs (low)

Dynamic VHDs have a footer *copy* at byte 0 and the authoritative footer at the
end of the file. The module doc implied the byte-0 copy was the sole footer.
Changed to "footer *copy* at byte 0 (the authoritative footer is the last 512
bytes)".

### Dynamic header magic mismatch returns generic `Io(InvalidData)` (medium)

When `parse_dynamic` found a cookie other than `b"cxsparse"` at the dynamic disk
header offset, it wrapped a string error in `Error::Io(io::Error::new(InvalidData,
...))`. Callers couldn't distinguish this structural parse failure from an OS I/O
error. Added a dedicated `Error::BadDynamicHeader` variant and used it here.
