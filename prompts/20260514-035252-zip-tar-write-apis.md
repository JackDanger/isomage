---
session: "zip-tar-write-apis"
timestamp: "2026-05-14T03:52:52Z"
model: claude-sonnet-4-6
---

## Human

Add write APIs for ZIP (stored) and TAR (ustar) archives, gated behind `--features write`. Extend the sequential-read benchmark to cover ZIP and TAR files. Fix clippy/fmt issues from the prior session.

## Assistant

### Key decisions

**ZIP write design**: Used a CRC-32 lookup table computed at compile time via `const fn`. This avoids any runtime initialization cost and keeps the write path allocation-free. The table uses the standard reflected polynomial `0xEDB88320`. The writer produces STORED (uncompressed) entries only — deflate compression is intentionally out of scope for isomage; it belongs in gzippy (the 7zippy deflate sibling).

**TAR write design**: Produces POSIX ustar archives (magic `ustar\0`, version `00`). The checksum is computed by filling the checksum field with spaces, summing all 512 header bytes, then writing the octal sum back — this matches both GNU tar and BSD tar conventions. Paths longer than 100 bytes are silently truncated; GNU long-name pre-headers (typeflag `L`) are deferred.

**Feature gate**: Both write functions live under `#[cfg(feature = "write")]` and are re-exported via `pub use write_impl::{...}` only when the feature is active. This keeps the read-only default build unaffected.

**Round-trip test strategy**: Each write test (a) calls `write_stored`/`write`, (b) optionally verifies with the system tool (`unzip -t`, `tar -tvf`) via `tools::UNZIP`/`tools::TAR` — skipped silently if the tool isn't on PATH — (c) parses back with `detect_and_parse`, and (d) checks tree structure and `cat_node` content. This pattern matches the existing round-trip test harness style.

**Benchmark extension**: Extended `corpus()` to accept `.zip` and `.tar` extensions. Dispatch in `bench_seqread` branches on extension and calls format-specific extract helpers (feature-gated). The 7-Zip baseline covers all formats uniformly so comparisons are apples-to-apples.

**`./` prefix fix in TAR reader**: TAR archives created with `tar -C $SRC_DIR -cf $IMAGE .` store all paths as `./filename`. The parser now strips the `./` prefix in `scan_entries` via `trim_start_matches("./")`. This is a systemic fix in the parser rather than a workaround in tests, because the `./` prefix is idiomatic in many real-world TARs.

**What I skipped**: Directory entries in TAR output (only file entries are written; parent directories are implicit). Long-path support via GNU extended headers. Deflate compression in ZIP (belongs in gzippy, not isomage).
