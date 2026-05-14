---
session: "coverage-multi-format"
timestamp: "2026-05-14T18:15:00Z"
model: claude-sonnet-4-6
---

## Human

Make sure all of our work is in PRs and/or merged and that there's fantastic tests with 100% coverage over everything.

## Assistant

### Key decisions

**`src/formats/squashfs.rs`**: Added 2 tests covering previously missed branches.

- `superblock_read_big_endian_magic_returns_bad_magic` — Writes `MAGIC_BE` bytes at offset 0, calls `Superblock::read()`. The magic is recognized as big-endian SquashFS (line 164 `false` branch), then rejected at the `!little_endian` guard (line 216 `return Err(Error::BadMagic)`). This covers the big-endian rejection path.

- `detect_inner_big_endian_magic_returns_ok` — `detect_inner` checks bytes 0-3 for either LE or BE magic and accepts both (line 618 condition). A BE-magic 4-byte buffer returns `Ok(())`. This covers the `be == MAGIC_BE` acceptance branch.

**What remains uncoverable in squashfs.rs**:
- Lines 155, 614: Non-EOF IO error arms — `Cursor<Vec<u8>>` never returns non-EOF errors.
- Lines 165, 174, 181, 197-206: Big-endian branches inside `u16_at`, `u32_at`, `u64_at` closures — these closures are defined before but only called after the `!little_endian` check at line 215 returns early, so the BE branches in the closures are dead code.
- Lines 558, 568: `)?;` error propagation LLVM artifacts from `parse_directory` and `build_tree` calls.

**`src/formats/zip.rs`** (no changes): Lines 403-422 are inside `const fn make_crc32_table()` — compile-time evaluated, LLVM runtime instrumentation cannot reach them. These are a hard ceiling.
