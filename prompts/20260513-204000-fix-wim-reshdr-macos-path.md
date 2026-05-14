---
session: "fix-wim-reshdr-macos-path"
timestamp: "2026-05-13T20:40:00Z"
model: claude-sonnet-4-6
---

## Human

Make sure all of our work is in PRs and/or merged and that there's fantastic tests with 100% coverage over everything. Release new versions as appropriate.

## Assistant

### Key decisions

**WIM RESHDR field order was wrong**: The module comment and `ResHdr::from_bytes` had the MS-WIM §2.3 RESHDR_DISK byte layout inverted. The correct layout is:
- bytes 0–7: CBDisk — compressed size in low 56 bits, flags in high 8 bits
- bytes 8–15: Offset — byte offset of resource from start of WIM file
- bytes 16–23: CBOriginal — uncompressed size

Our code had the offset extracted from bytes 0–7 (actually the compressed size) and bytes 8–15 named `size` (actually the offset). This made all real WIM files from `wimlib-imagex` fail with `Error::TooShort` because the code was seeking to the compressed_size value as if it were a file offset, and the bounds check `original_size > file_size - offset` was triggering.

The unit tests all passed because `build_wim` was also writing fields in the wrong order — so the parser and builder agreed on the wrong layout.

Fix: correct `ResHdr::from_bytes` to read offset from bytes 8–15. Update `build_wim` test helper to write the offset to bytes 8–15 and compressed_size to bytes 0–7. The flags byte (byte 7 of the first field) and the compressed XML test remain unchanged.

**macOS CI rustup-init interference**: The `brew install` step in the `round-trip (macos-latest)` job shadows the `cargo` binary installed by `dtolnay/rust-toolchain`. Subsequent `cargo test` calls hit a `rustup-init` stub that rejects the `test` subcommand with "unexpected argument 'test' found". Fixed by adding a "Reinstate Rust toolchain PATH" step (macOS-only) immediately after brew install that prepends `$HOME/.cargo/bin` to `$GITHUB_PATH`.
