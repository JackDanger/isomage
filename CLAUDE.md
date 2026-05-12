# Working in isomage

This repository is a **pure-Rust library** for reading and (as of
v3.0) writing disc and disk images. The matrix is broader than the
name suggests — ISO 9660 and UDF remain the centerpiece, but the
crate is the umbrella for raw IMG / partition tables / VHD / VHDX /
VMDK / QCOW2 / WIM / DMG / FAT / exFAT / NTFS / ext / HFS+ /
SquashFS / APFS support. There is no CLI binary, no Homebrew tap,
no distributed executable — the only artifact is the crate on
crates.io and its rustdoc on docs.rs.

## Hard rules, read first

1. **Promptlog gate.** If your changes touch any file under `src/`
   or `Cargo.toml`, you **MUST** add a new file at
   `prompts/YYYYMMDD-HHMMSS-<slug>.md` before opening a pull
   request. The `prompt-log` job in `.github/workflows/ci.yml`
   fails the PR otherwise. Treat it the same as "tests must pass."

2. **No runtime deps in `default` features.** `cargo build` with
   the default feature set still pulls zero runtime crates. New
   deps are allowed *only* behind a non-default cargo feature
   (`mmap`, `simd`, codec features like `deflate-gzippy`/`lzma`/
   `zstd`/`bzip2`/`lz4`, or a format feature). Adding a dep — even
   behind a feature — requires the PR description to explain why
   the format / perf path can't be done without it. See the
   v3-scope-expansion prompt log for the rationale.

3. **No CLI, no binary.** Don't reintroduce `[[bin]]` in
   `Cargo.toml` or `src/main.rs`. The project explicitly does not
   distribute an executable; if a contributor needs a CLI they can
   wrap the public API themselves (see README's "If you want a CLI"
   section). Pre-2.0 versions did ship one; do not bring it back.

4. **`unsafe` only behind a feature, only in hot paths.** Default-
   feature builds remain 100% safe Rust. `unsafe` is permitted
   inside the `mmap` and `simd` modules (and nowhere else),
   contained to small blocks, and exercised by Miri in CI before
   release. Every `unsafe` block carries a `// SAFETY:` comment.

5. **Never write to the *input* image.** Write APIs create *new*
   images; the reader path is read-only forever. Write APIs are
   gated behind `--features write` until fsck-clean validation
   lands per format.

6. **Fsck-clean writes.** Any new write target must round-trip
   against the matching reference tool (`mkisofs -check-session`,
   `fsck.vfat`, `qemu-img check`, etc.) in CI before merging.
   Untested write code stays behind `#[cfg(feature = "experimental")]`.

## How to add a prompt log correctly

1. **Read** `prompts/PROMPTLOG.md` — the format spec, with full examples.
2. **Use the skill** at `.claude/skills/promptlog.md` — it's a
   step-by-step walkthrough. Invoke it as `/promptlog` if your
   harness supports skill invocation; otherwise read the file
   directly and follow the steps.
3. **Sanitize**: redact secrets, internal URLs, customer data, PII.
   The test is "would you be comfortable if this appeared on the
   front page of Hacker News?"
4. **Commit the prompt log file in the same commit as the code
   change**, or as an immediate follow-up commit. CI re-runs on
   every push.

## What if I'm a worktree agent that can't commit?

Then the orchestrator (the conversation that spawned you) is
responsible for committing the prompt log on your behalf, in a
follow-up commit on your branch. Surface in your final message:
"I changed `src/foo.rs` — the orchestrator should add a prompt log
entry covering these decisions: [bullet list of the key decisions]."
The orchestrator will use the `promptlog` skill.

If you _can_ commit, write the prompt log yourself.

## Local pre-commit hook (optional but recommended)

Install once per clone:

```sh
./scripts/install-hooks.sh
```

This installs `.git/hooks/pre-commit`, which mirrors the CI gate.
To bypass for a single intentional commit (e.g. worktree-agent code
that gets a follow-up prompt log commit):

```sh
SKIP_PROMPT_LOG=1 git commit -m "..."
```

Do not routinely skip the hook.

## Project facts

- **Type**: pure-Rust library crate (`[lib]` only).
- **Public API (v2.x, still current)**: `detect_and_parse_filesystem`,
  `detect_and_parse_filesystem_verbose`, `cat_node`, `extract_node`,
  `TreeNode`, plus the `iso9660` and `udf` submodules and the
  `Error` / `Result` type aliases. All documented; all on
  [docs.rs/isomage](https://docs.rs/isomage).
- **Public API (v3.0, pending)**: adds a unified `Image::open` /
  `Image::create` facade, the `RandomAccess` trait, `MmapImage`,
  and a per-format submodule under `src/formats/`. `TreeNode.name`
  becomes `Cow<'_, str>` (breaking).
- **Layout**:
  - `src/lib.rs` — public API + crate-level rustdoc.
  - `src/iso9660.rs` — ISO 9660 (with Joliet, Rock Ridge) parser.
  - `src/udf.rs` — UDF parser (incl. metadata partitions, multi-extent files).
  - `src/tree.rs` — `TreeNode`, the wire format between parsers and consumers.
  - `src/io/` — IO traits and `MmapImage` (added v3.0, `mmap` feature).
  - `src/simd/` — SIMD CRC routines (added v3.0, `simd` feature).
  - `src/formats/` — one submodule per supported image/filesystem
    format (added v3.0, each behind its own feature).
- **Tests**:
  - **Unit tests** live as `#[cfg(test)] mod tests` inline in
    `src/lib.rs` and each format submodule. Doc-tests in
    `src/lib.rs` and `src/tree.rs` cover the README examples.
  - **Integration / round-trip tests** live under `tests/`
    (v3.0 addition). One file per format
    (`tests/<format>_round_trip.rs`) plus the harness self-test
    (`tests/harness_self_test.rs`). Shared infrastructure
    (Tool detection, RoundTrip builder, assertions, snapshot
    rendering) is in `tests/common/`.
  - Generated test ISOs live under `test_data/` and are produced
    by `make test-data`. The `tests/` tree is excluded from
    crates.io publish via `Cargo.toml`'s `include = […]` allow-list.
- **Build & test**: `cargo build`, `cargo test`. CI also runs
  `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo doc --no-deps` with `RUSTDOCFLAGS="-D warnings"`, an MSRV
  build (1.74), `cargo audit`, and a `cargo package` contents check
  to prevent accidental publish of `prompts/` / `scripts/` /
  `test_data/`.
- **Release**: tag `vX.Y.Z`, push the tag. The release workflow
  creates a GitHub Release with auto-generated notes and runs
  `cargo publish`. No platform binaries.

## House style

- Match the surrounding code: comment density, naming, idiom. The
  parsers in `iso9660.rs` and `udf.rs` favor explicit byte offsets
  and inline comments citing the ECMA-119 / ECMA-167 spec sections.
  Keep that style when extending them.
- Verbose / progress / error output goes to **stderr**. Functions
  that take a writer (`cat_node`) write *only* file bytes to it —
  no headers, no framing, no progress text.
- Read-only on the *input* path, always. Write APIs are additive,
  gated behind `--features write`, and must round-trip against a
  reference tool. See `prompts/20260512-193754-v3-scope-expansion.md`
  for the v3.0 rationale.
- Names matter at the publish boundary. Re-renaming a `pub` item is
  a breaking change; deprecate first, then rename in a major
  release.
