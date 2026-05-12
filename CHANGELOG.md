# Changelog

All notable changes to `isomage` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the
project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.1.0] — 2026-05-12

### Security

- **Path-traversal hardening in `extract_node`.** Every entry name is now
  validated to reject empty strings, `.`, `..`, and any name containing
  `/`, `\`, or NUL bytes. As defense in depth, the output directory is
  canonicalized once at entry and every resolved write path is checked to
  stay under it. An adversarial ISO with a Rock Ridge `NM` record or UDF
  FID claiming a name like `../../etc/passwd` now produces a clear error
  rather than silently writing outside the destination. Four regression
  tests cover dotdot, slash, absolute, and NUL byte cases plus a unit
  test on `validate_entry_name`.

### Fixed

- **`-c` no longer fails on broken pipes.** `cat_node` catches
  `io::ErrorKind::BrokenPipe` from the writer and returns `Ok(())` —
  matching the standard Unix `| head` semantics. Cross-platform
  (no signal handler dependency); regression test injects a `Write`
  impl that returns `BrokenPipe`.
- **Extract loops no longer truncate on 32-bit.** The `length as usize`
  cast in the extract/cat loops is gone; `remaining` stays `u64` and is
  clamped to `EXTRACT_CHUNK_SIZE` (a `usize` constant) before each cast.
  Files ≥ 4 GB no longer silently truncate when extracted from a 32-bit
  build.

### Added — CLI

- `isomage --completions <SHELL>` prints a shell-completion script
  (bash, zsh, fish, powershell, elvish) to stdout.
- `isomage --man-page` prints a groff(7) man page to stdout.
- Both flags are `exclusive` and don't require an `IMAGE` argument, so
  packaging scripts can run them without producing a usage error.
- Release tarballs now bundle `isomage.1` and `isomage.bash` /
  `isomage.zsh` / `isomage.fish` alongside the platform-suffixed binary.
- Homebrew installs now place the man page in `man1/` and the completion
  scripts in each shell's expected directory; `man isomage` works
  directly after `brew install`.

### Added — Library

- New public type aliases `isomage::Error` (boxed `dyn Error + Send +
  Sync + 'static`) and `isomage::Result<T>`. Send+Sync makes the error
  composable with threads, async, and `anyhow`.
- Comprehensive `///` doc comments on every public function, struct,
  field, and method, plus a crate-level overview. Five doc-tests
  exercise the README examples and prevent doc-rot.
- `documentation = "https://docs.rs/isomage"` and `rust-version = "1.74"`
  declared in `Cargo.toml`.
- `Cargo.toml` `include = […]` restricts the crates.io publish to source
  + manifests + `LICENSE` + `README.md` + `CHANGELOG.md`; prompt logs,
  test data, and scripts are no longer uploaded.

### Added — Tooling

- `.github/workflows/ci.yml` gained six new gates: `fmt`, `clippy`,
  `docs`, `msrv`, `audit`, and a release-binary `smoke` test that drives
  `--version`, `--help`, list/cat/extract, `--completions`, and
  `--man-page` end to end. Plus a workflow-level `concurrency` group and
  `Swatinem/rust-cache` on every job.
- `CHANGELOG.md` (this file), `SECURITY.md`, GitHub
  `.github/ISSUE_TEMPLATE/*`, and `.github/pull_request_template.md`
  with a promptlog reminder.

### Changed — possibly breaking

- The public functions in `isomage::` now return `isomage::Result<T>`,
  which is `Result<T, Box<dyn Error + Send + Sync + 'static>>`. This is
  a narrowing of the old `Result<T, Box<dyn Error>>`: callers who
  explicitly typed the boxed error without `Send + Sync` will need to
  adjust. The CLI is unaffected.

## [1.0.0] — 2026-05-11

### Added

- Adopted the [promptlog pattern](https://jackdanger.com/promptlog/).
  Every PR that changes `src/` or `Cargo.toml` must add a sanitized
  log of the prompts that led to the change under `prompts/`. The CI
  job `Prompt log check` enforces it.
- `prompts/PROMPTLOG.md` — full spec, sanitization rules, two-commit
  worktree-agent pattern, "never compact existing prompt files" rule.
- `.claude/skills/promptlog.md` — step-by-step skill (`/promptlog`) for
  AI agents.
- `CLAUDE.md` — root-level agent rules.
- `CONTRIBUTING.md` — human contributor guide.
- `scripts/check_promptlog.sh` — the check used by both CI and the
  local pre-commit hook.
- `scripts/install-hooks.sh` — installs a worktree-aware pre-commit
  hook that mirrors the CI gate.
- README rewritten for human + agent comprehension: Architecture
  section mapping each `src/*.rs` module, Output Contract table,
  seven numbered Invariants, and an Extension Points table.

### Changed

- Project now publishes both the CLI binary and the library from the
  same crate; both audiences install via `cargo install isomage` and
  `isomage = "1"` respectively.

## [0.4.0] — 2026-05-11

Pre-1.0 release. See git log for details.

[1.1.0]: https://github.com/JackDanger/isomage/releases/tag/v1.1.0
[1.0.0]: https://github.com/JackDanger/isomage/releases/tag/v1.0.0
[0.4.0]: https://github.com/JackDanger/isomage/releases/tag/v0.4.0
