---
session: "v2.0.0-library-only"
timestamp: "2026-05-12T06:40:33Z"
model: claude-opus-4-7
sequence: 1
---

## Human

After reflection on the v1.0 audit, the user asked whether `isomage`
does anything `7z` doesn't. Honest answer: the unique value is the
pure-Rust library, not the CLI. User decided: "Let's do just library.
Never release as executable. Polish, but polish as a pure-Rust way to
manage reading images." Also: "Open / edit / close any PRs necessary
to do this." PR #8 (which mixed lib improvements with completions /
man-page / Homebrew updates) was closed; this PR is v2.0.0,
library-only.

## Assistant

### Key decisions, commit 1: pull the lib-side fixes from PR #8

The four lib source files (`src/lib.rs`, `src/iso9660.rs`,
`src/udf.rs`, `src/tree.rs`) were already polished in PR #8's first
commit. Rather than redo them, I `git checkout f3d9b46 -- src/...`'d
them into this fresh worktree from v1.0.0. The changes are
unaltered from the audit in sequence 1 of v1.1.0-spruce-up:

- **Path traversal hardening in `extract_node`**: layered defense.
  `validate_entry_name` rejects empty / `.` / `..` / `/` / `\` /
  NUL; `safe_join` re-checks `starts_with(canonical_root)`; the
  output dir is canonicalized once at entry. Four regression tests
  plus a unit test for `validate_entry_name`.
- **`cat_node` BrokenPipe tolerance**: cross-platform `Ok(())` on
  `ErrorKind::BrokenPipe` from the writer.
- **`u64` length clamp before `usize` cast** in both cat and extract
  loops; 32-bit safe for files ≥ 4 GB.
- **Public `isomage::Error` / `isomage::Result<T>` aliases** with
  `Send + Sync + 'static` on the boxed error. Internal per-module
  `Result` aliases in `iso9660.rs` and `udf.rs` now refer to
  `crate::Result`, propagating Send+Sync.
- **`///` doc comments on every `pub` item**, plus a crate-level
  overview and five doc-tests.
- **Three clippy lints fixed** (`if_same_then_else` in `iso9660.rs`
  collapsed with a spec-cited comment; `manual_div_ceil` in
  `udf.rs`; `manual_range_contains` in both).
- **`cargo fmt --all`** applied; whitespace-only changes.

These changes are valuable regardless of CLI vs. library
positioning. They land first as a standalone commit so the diff is
reviewable on its own merits.

### What I skipped

- A typed `IsoError` enum. Box-Send-Sync is the right v2 move; a
  typed enum could be v3.
- Implementing Rock Ridge permissions or timestamps on extracted
  files. Real features, deserve their own PR.

### Verified

- `cargo test`: 30 lib tests pass (incl. the 6 new security
  regressions and the BrokenPipe test), 5 doc-tests pass.
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo fmt --all --check`: clean.
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`: clean.
