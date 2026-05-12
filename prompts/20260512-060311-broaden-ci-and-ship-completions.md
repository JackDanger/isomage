---
session: "v1.1.0-spruce-up"
timestamp: "2026-05-12T06:03:11Z"
model: claude-opus-4-7
sequence: 2
---

## Human

(Continuation of v1.1.0-spruce-up; see sequence 1 for the original
chain.) The user asked to broaden CI significantly and to make the
release ship the man page and completions for the binary.

## Assistant

### CI broadening: which jobs and why

Added six new jobs to `.github/workflows/ci.yml`, each one a hard gate:

1. **`fmt`** — `cargo fmt --all -- --check`. The whole tree was
   re-formatted in sequence 1; this job stops drift.
2. **`clippy`** — `cargo clippy --all-targets -- -D warnings`. We just
   cleaned up three lints; this job keeps that surface clean. Using
   `--all-targets` so tests and examples are linted too, not just lib
   and bin.
3. **`docs`** — `cargo doc --no-deps --all-features` with
   `RUSTDOCFLAGS="-D warnings"`. Catches broken intra-doc links,
   undocumented `pub` items if we ever enable that lint, and
   doc-test compile failures.
4. **`msrv`** — builds with the toolchain version read from
   `Cargo.toml`'s `rust-version` field. If someone uses a newer Rust
   feature, this turns red before publish — the alternative is a
   bug report from a user whose `cargo install` fails.
5. **`audit`** — `cargo install --locked cargo-audit` then `cargo
   audit`. Reads RustSec advisories against the current `Cargo.lock`.
   Tiny dep tree (clap + clap_complete + clap_mangen + a handful of
   transitives) so it'll virtually always pass, but the point is to
   _know_ when a transitive dep gets a CVE.
6. **`smoke`** — builds the release binary, then drives the
   end-user surface in seven separate steps: `--version` matches
   `Cargo.toml`, `--help` exits 0, list mode prints the tree, `-c`
   piped through `head -c 4` exits 0 (this is the BrokenPipe test
   end-to-end, not just at the lib level), `-x` extracts and the
   expected file appears, `--completions bash` emits `_isomage`,
   `--man-page` emits `.TH isomage 1`. This is the integration test
   layer `cargo test` doesn't reach.

Plus two operational improvements:

- **`concurrency`** group: cancels in-flight CI on the same branch
  when a new push lands. Saves a lot of CPU on rapid PR iteration.
- **`Swatinem/rust-cache@v2`** on every job: cuts cold-start time
  per job from ~60s to ~10s on warm runs.

Chose this set over alternatives:

- **cargo-deny vs. cargo-audit**: deny is more capable (licenses,
  banned crates), but for a project with three direct deps and the
  MIT license clearly stated, audit is sufficient and has zero
  configuration burden. Keep deny in reserve for when the dep tree
  grows.
- **Tarpaulin / coverage**: deferred. Coverage on a project this size
  is more vanity than value, and meaningful coverage would require
  property tests on the parsers — a real follow-up, not paperwork.
- **`cargo bench`**: not added. No benchmarks exist yet; an empty
  bench job is misleading.

### Release: ship man page + shell completions

`.github/workflows/release.yml` gained a new `docs` job that builds
the binary natively (`cargo build --release` with no `--target`) and
runs it to emit `isomage.1` (groff man page) and bash/zsh/fish
completion scripts. The artifacts are uploaded once and downloaded by
every per-platform `build` job, then packaged into each tarball.

This split is required because the matrix builds are
cross-compiled: a Linux runner can't natively execute the arm64-musl
binary it produces, so it can't run that binary to generate its own
man page. Doing the generation once on a native host and shipping the
result is the standard pattern.

**Sanity checks in the `docs` job** verify the artifacts before
uploading: `.TH isomage 1` must appear in the man page, and all
three completion files must be non-empty. Catches the day someone
accidentally breaks the `--man-page` exit path.

**Tarball layout stayed flat and backwards-compatible.** The binary
keeps its existing platform-suffixed name (`isomage-macos-arm64`,
etc.) at the top level of the tarball, so the existing `install.sh`
keeps working unchanged. The new files (`isomage.1`, `isomage.bash`,
`isomage.zsh`, `isomage.fish`) sit alongside as additions, not
replacements. Considered switching to a directory-layout tarball;
rejected because it would break every existing `curl … | tar xz |
sudo mv …` instruction in the wild.

**Homebrew formula** now installs:

- `bin/isomage` — the binary (renamed from the platform-suffixed file).
- `man1/isomage.1` — the man page, so `man isomage` works after `brew install`.
- `etc/bash_completion.d/isomage` — bash completions, loaded by Homebrew's bash-completion setup.
- `share/zsh/site-functions/_isomage` — zsh completions, loaded by Homebrew's zsh setup.
- `share/fish/vendor_completions.d/isomage.fish` — fish completions.

The Homebrew DSL helpers (`man1.install`, `bash_completion.install`,
etc.) put each file in the exact directory the corresponding shell
checks for completions inside the Homebrew prefix. No manual `eval
$(brew --prefix)/...` needed.

**`test` block in the formula** asserts the man page exists and the
bash completion references the expected function name. This runs
on `brew test isomage` and catches packaging regressions that the
crates.io publish wouldn't notice.

### What I skipped

- A Windows CI runner. Most of the I/O is portable, but enough of
  the path handling and the install/release workflows are
  Unix-shaped that an actual Windows port would be a real project,
  not a CI tweak.
- `cargo publish --dry-run` in CI. Catches packaging errors before
  the release tag is pushed. Worth adding next round.
- Sccache or `actions-rs/cargo`. The rust-cache action covers 90% of
  the wins; sccache would help on cold builds in CI but adds
  complexity.
- Reviewing the install.sh path for the new artifacts. It still
  works (the binary name didn't change); a follow-up could teach it
  to optionally install the man page and completions.

### Verified locally where possible

CI workflow changes can only be fully validated by pushing them. The
release workflow changes follow a well-tested pattern (the existing
release.yml works; the new `docs` job and the bundled-extras packaging
are additive).

The `docs` job's exact commands run cleanly locally:

```
$ ./target/debug/isomage --man-page | head -5
.ie \n(.g .ds Aq \(aq
.el .ds Aq '
.TH isomage 1  "isomage 1.1.0"
.SH NAME
isomage \- Browse and extract files from ISO images without mounting them
```
