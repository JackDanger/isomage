---
session: "v2.0.0-library-only"
timestamp: "2026-05-12T06:40:33Z"
model: claude-opus-4-7
sequence: 2
---

## Human

(Continuation of v2.0.0-library-only; see sequence 1.) After landing
the lib-side fixes, remove every CLI / binary-distribution surface
that this project no longer wants to maintain. The user was explicit:
"Never release as executable. Polish, but polish as a pure-Rust way
to manage reading images."

## Assistant

### Key decisions

**`src/main.rs` deleted, `[[bin]]` removed from `Cargo.toml`.** Not
moved to `examples/`; just gone. The README's "If you want a CLI"
section reproduces the previous behaviour in ~50 lines on top of the
public API for anyone who needs it. Keeping example code in the tree
would mean keeping it building, keeping it documented, fielding
contributor PRs against it — at which point it's a CLI again, just
hidden.

**Zero runtime dependencies.** `clap` was the only entry in
`[dependencies]` and it was bin-only. Cargo.lock now has one entry:
`isomage` itself. This becomes invariant 7 in the README (and a
contributor rule in CLAUDE.md / CONTRIBUTING.md): adding a runtime
dep needs justification in the PR description. The point of being a
small pure-Rust crate is that downstream can adopt it without
auditing a tree.

**`install.sh` deleted.** Library users `cargo add isomage`. The
shell installer was for the CLI tarballs that no longer exist.

**`doc/demo.svg` deleted.** It animated the CLI; without one, it
documents a feature the library doesn't have.

**Makefile trimmed to `test-data` plus thin `build`/`test`/`clean`
wrappers.** The cross-platform build targets (`build-macos`,
`build-linux`, `build-release`, `install-targets`) all referenced
the now-deleted `[[bin]]`. Keeping `test-data` is essential: the
synthetic test ISOs drive every integration test in `cargo test`.

**`release.yml` shrunken from seven jobs to two.** Was: native docs
job + four cross-compiled binary builds + GitHub Release + crates.io
publish + Homebrew formula update. Now: a `github-release` job for
the tag landing page (with auto-generated notes and a `[dependencies]
isomage = "X.Y.Z"` snippet), plus a `publish-crate` job. The
`publish-crate` job also verifies the tag matches the `Cargo.toml`
version before pushing — turns a class of mistake (forgetting to
bump `Cargo.toml`) into a CI failure instead of a half-released
state.

The Homebrew tap's existing v1.0.0 formula stays as-is in the
external tap repo. Removing it would require pushing to a different
repo from this workflow; it's a manual cleanup the maintainer can
do later. The formula will simply not auto-update on v2.0.0+.

**`ci.yml`** keeps every gate that's still relevant — test (macOS +
Ubuntu matrix), fmt, clippy, docs, MSRV, audit, the prompt-log
check, and a new `package` job that runs `cargo package` and asserts
no forbidden paths (`prompts/`, `scripts/`, `test_data/`, `.claude/`,
`.github/`, `install.sh`, `Makefile`) leaked into the published
tarball. The `smoke` job from PR #8 is gone — it was a release-binary
test for a release binary we no longer produce.

**Cargo.toml `include = [...]`** restricts the publish to source,
manifests, LICENSE, README, CHANGELOG. With this in place,
`cargo package` produces an 11-file, ~28 KB tarball.

**Cargo.toml metadata** updated for library identity:

- `description`: "Pure-Rust reader for ISO 9660 (Joliet, Rock Ridge)
  and UDF disc images. Read-only, no mount, no FUSE."
- `categories`: `["filesystem", "parser-implementations"]` — drops
  `command-line-utilities`.
- `documentation = "https://docs.rs/isomage"`.
- `rust-version = "1.74"` (was implicit before).
- `version = "2.0.0"` — the removal of `[[bin]]` is a breaking
  change for `cargo install isomage` users; honest semver.

### What I skipped

- Yanking v1.0.0 / v1.1.0 from crates.io. Yanking is for security
  issues, not packaging shape changes; v1.x stays installable for
  anyone with it in their Cargo.lock.
- Deleting the Homebrew tap's `isomage.rb` formula. Requires
  pushing to an external repo; out of this PR's reach.
- Cleaning up old GitHub Releases (the platform tarballs for
  v0.1.0 through v1.0.0). They were promised distribution
  artifacts; leaving them up is honest.

### Verified

- `cargo build`: clean.
- `cargo test`: 30 + 5 tests pass.
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo fmt --all --check`: clean.
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps`: clean.
- `cargo audit`: 1 dependency (`isomage`), 0 vulnerabilities.
- `cargo package --no-verify --allow-dirty`: 11 files, 27.5 KiB compressed.
  Forbidden-paths check passes.
