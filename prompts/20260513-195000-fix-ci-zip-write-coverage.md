---
session: "fix-ci-zip-write-coverage"
timestamp: "2026-05-13T19:50:00Z"
model: claude-sonnet-4-6
---

## Human

Make sure all of our work is in PRs and/or merged and that there's fantastic tests with 100% coverage over everything. Release new versions as appropriate.

## Assistant

### Key decisions

**CI write feature missing**: `tar_write_round_trip` and `zip_write_round_trip` require the `write` feature via Cargo.toml `required-features`, but the round-trip CI job was running without `write`. Added `write` to all four `cargo test --features` invocations in `.github/workflows/ci.yml` (build step + Ubuntu strict + macOS non-strict + Docker pinned).

**dmg_round_trip.rs unused imports on Linux**: `Seek`, `SeekFrom`, `common::tools`, and `common::RoundTrip` were imported unconditionally but only used inside `#[cfg(target_os = "macos")]` functions. Fixed by gating those imports with `#[cfg(target_os = "macos")]`.

**zip write_impl unit test coverage**: The write module in `src/formats/zip.rs` (lines 403–519) was only exercised by integration tests (`tests/zip_write_round_trip.rs`), so `cargo llvm-cov --lib` reported ~100 uncovered lines there. Added a `#[cfg(all(test, feature = "write"))]` sub-module with tests for: `crc32` known vectors, `write_stored` single file, multiple files, empty entries, and nested paths.

**zip edge case tests**: Added tests for two previously-uncovered branches:
- `find_eocd_rejects_too_short`: file shorter than EOCD_MIN_SIZE (22 bytes) → NotZip
- `entry_with_slash_only_name_skipped`: CD entry with name "/" → raw.is_empty() after trim → skipped, tree has 0 children
- `entry_with_doubled_slash_in_path`: "foo//bar.txt" → empty component skipped → resolves to /foo/bar.txt
