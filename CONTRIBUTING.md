# Contributing to isomage

Thanks for considering a contribution. `isomage` is a small, focused
pure-Rust library; PRs that keep it small, focused, and dependency-free
are the easiest to land.

## Before you open a PR

1. **Build and test**:
   ```sh
   make test-data
   cargo build
   cargo test
   ```
2. **Run the prompt-log check locally** (installs a pre-commit hook the
   first time):
   ```sh
   ./scripts/install-hooks.sh
   ```
3. **Write a prompt log entry** describing the human prompts and key
   decisions in your change. See [`prompts/PROMPTLOG.md`](prompts/PROMPTLOG.md)
   for the spec, or invoke the `/promptlog` skill if you're using
   Claude Code.

## The promptlog pattern

isomage commits the prompts that led to each change, alongside the code.
This is the [promptlog pattern](https://jackdanger.com/promptlog/). It
applies whether you wrote the code yourself, paired with an AI agent, or
the agent did most of the work.

The CI gate fails any PR that changes `src/` or `Cargo.toml` without
adding a file under `prompts/`. The format is documented in
[`prompts/PROMPTLOG.md`](prompts/PROMPTLOG.md); a step-by-step skill
for AI agents lives at [`.claude/skills/promptlog.md`](.claude/skills/promptlog.md).

**You don't have to use an AI agent to contribute.** If you wrote the
code yourself, the prompt log is a short rationale document — what you
set out to change, and why you made the design choices you did. Same
format, no "Assistant" section required if there wasn't one.

## What kinds of changes are welcome

- **Bug fixes** with a regression test. ISO 9660 / UDF parsing is full
  of edge cases; the easiest way to make this library better is to
  bring an image it can't read and a test that proves the fix.
- **Spec coverage**. Joliet, Rock Ridge, multi-extent files, metadata
  partitions, El Torito boot records — all in scope. New
  filesystems (HFS+, exFAT, etc.) require a real use case first, but
  not a no.
- **API polish**: better doc comments, clearer error messages, more
  doc-tests covering the README examples, type signatures that are
  easier to use from `anyhow` / `tokio` / threaded code.
- **Performance**, especially around the cat/extract chunked I/O
  loops or path-table traversal.
- **Documentation**, especially around the file formats — inline
  spec-section comments in the parser source are the style.

## What's out of scope

- **Writing to ISO images.** `isomage` is read-only by design.
- **Mount-style integrations** (FUSE, kernel modules, virtual filesystems).
- **Distributing a CLI executable.** v1.x shipped one; v2.0.0
  removed it. The maintainer is not distributing platform
  binaries, a Homebrew formula, or a `cargo install` target. If you
  want a CLI, the README's "If you want a CLI" section reproduces
  the previous behaviour in ~50 lines on top of the public API. A
  separate maintained CLI crate is welcome — under a different
  name.
- **Adding runtime dependencies** without a real justification. The
  crate is zero-dep on purpose (invariant 7 in the README); adding a
  `[dependencies]` entry needs to explain in the PR why a stdlib
  implementation won't work.
- **Adding `unsafe`.** The parsers are safe end-to-end. Don't change
  that without a discussion.

## Questions

Open an issue or start a draft PR. Both are good ways to check
whether a change is in scope before you invest in it.
