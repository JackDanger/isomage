# Changelog

All notable changes to `isomage` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the
project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [2.0.0] — 2026-05-12

### Repositioning

**`isomage` is now a library-only crate.** The previous CLI binary
has been removed; the crate publishes only `[lib]`. If you need a
command-line tool, the README's "If you want a CLI" section
reproduces the old behaviour in ~50 lines on top of the public API.

**Why**: `7z` and friends already cover the general-purpose ISO/UDF
extraction use case. The unique value here is "a pure-Rust crate
that parses ISO 9660 + UDF, with no dependencies, no `unsafe`, and
a public API you can read in an afternoon." That's a library
identity, not a CLI identity. The split makes maintenance simpler
and the docs.rs page becomes the project's primary surface.

### Removed — BREAKING

- **`[[bin]]` target.** `cargo install isomage` no longer installs a
  binary on `$PATH`. Existing v1 installations on user machines are
  unaffected. Anyone scripting against the CLI should pin to `v1.0.0`
  in their installer, or migrate to the 50-line wrapper in
  README.md.
- **GitHub-Releases binaries** (macOS x86_64/arm64, Linux x86_64/arm64
  musl). The v1.0.0 release assets remain available; v2.0.0 onwards
  has no platform binaries.
- **Homebrew tap formula.** `brew install jackdanger/tap/isomage`
  will keep working off the v1.0.0 formula already in the tap (the
  formula is in an external repo and is not automatically updated
  by this release). The maintainer may remove the formula manually
  in a future cleanup.
- **`install.sh`.** Deleted from the repo. Library users install via
  `cargo add isomage`.
- **CLI tooling-integration flags** (`--completions`, `--man-page`).
  Belonged to the binary; gone with it.

### Security

- **Path-traversal hardening in `extract_node`.** Every entry name is
  validated to reject empty strings, `.`, `..`, and any name containing
  `/`, `\`, or NUL bytes. As defense in depth, the output directory is
  canonicalized once at entry and every resolved write path is checked
  to stay under it. An adversarial ISO with a Rock Ridge `NM` record or
  UDF FID claiming a name like `../../etc/passwd` now produces a clear
  error rather than silently writing outside the destination. Four
  regression tests cover dotdot, slash, absolute, and NUL byte cases
  plus a unit test on `validate_entry_name`.

### Fixed

- **`cat_node` no longer fails on broken pipes.** Catches
  `io::ErrorKind::BrokenPipe` from the writer and returns `Ok(())` —
  matching standard Unix `| head` semantics. Cross-platform (no signal
  handler dependency); regression test injects a `Write` impl that
  returns `BrokenPipe`.
- **Extract / cat loops no longer truncate on 32-bit targets.** The
  `length as usize` cast in the read loops is gone; `remaining` stays
  `u64` and is clamped to `EXTRACT_CHUNK_SIZE` (a `usize` constant)
  before each cast.

### Added

- **`isomage::Error`** is `Box<dyn std::error::Error + Send + Sync + 'static>`;
  **`isomage::Result<T>`** is its `Result` alias. Send+Sync makes the
  error usable with threads, async, and `anyhow`.
- **Crate-level rustdoc** on `src/lib.rs`, plus `///` doc comments on
  every public function, struct, field, and method. Five doc-tests
  exercise the README examples and protect them from rotting.
- **`documentation = "https://docs.rs/isomage"`** and
  **`rust-version = "1.74"`** declared in `Cargo.toml`.
- **`include = […]`** in `Cargo.toml` restricts the crates.io publish
  to source + manifests + `LICENSE` + `README.md` + `CHANGELOG.md`. The
  release tarball is now 10 files, ~25 KB compressed.
- **CI broadening**: new `fmt`, `clippy --all-targets -D warnings`,
  `docs` (with `RUSTDOCFLAGS="-D warnings"`), `msrv` (builds on the
  declared minimum), `audit` (`cargo audit` against `Cargo.lock`), and
  `package` (dry-run `cargo publish` plus a forbidden-paths check
  that fails if `prompts/`, `scripts/`, or test data leak into the
  tarball). Plus a workflow-level `concurrency` group and
  `Swatinem/rust-cache` on every job.
- **`CHANGELOG.md`** (this file), **`SECURITY.md`**, GitHub
  `.github/ISSUE_TEMPLATE/*`, and `.github/pull_request_template.md`
  with a promptlog reminder.

### Changed — internal

- Per-module `Result` aliases in `iso9660.rs` and `udf.rs` now refer
  to `crate::Result`, deduplicating and propagating Send+Sync.
- Three pre-existing clippy lints fixed (`if_same_then_else` in
  `iso9660.rs` collapsed with a spec-cited comment;
  `manual_div_ceil` and `manual_range_contains` modernized).
- `cargo fmt --all` applied to the whole tree; whitespace-only.

## [1.0.0] — 2026-05-11

### Added

- Adopted the [promptlog pattern](https://jackdanger.com/promptlog/):
  every PR that changes `src/` or `Cargo.toml` adds a sanitized log
  of the prompts that led to the change under `prompts/`. The CI
  `Prompt log check` job enforces it.
- `prompts/PROMPTLOG.md` — full spec, sanitization rules, two-commit
  worktree-agent pattern, "never compact existing prompt files" rule.
- `.claude/skills/promptlog.md` — step-by-step skill (`/promptlog`)
  for AI agents.
- `CLAUDE.md` — root-level agent rules.
- `CONTRIBUTING.md` — human contributor guide.
- `scripts/check_promptlog.sh` — the check used by both CI and the
  local pre-commit hook.
- `scripts/install-hooks.sh` — installs a worktree-aware pre-commit
  hook that mirrors the CI gate.
- README rewritten for human + agent comprehension: Architecture
  section mapping each `src/*.rs` module, Output Contract table,
  seven numbered Invariants, and an Extension Points table.

This is the last release that included a CLI binary distribution.

## [0.4.0] — 2026-05-11

Pre-1.0 release. See git log for details.

[2.0.0]: https://github.com/JackDanger/isomage/releases/tag/v2.0.0
[1.0.0]: https://github.com/JackDanger/isomage/releases/tag/v1.0.0
[0.4.0]: https://github.com/JackDanger/isomage/releases/tag/v0.4.0
