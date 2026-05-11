# Working in isomage

This repository follows the **promptlog pattern**: every PR that changes
source code must add a sanitized log of the prompts that led to it.

## Hard rule, read first

If your changes touch any file under `src/` or `Cargo.toml`, you **MUST**
add a new file at `prompts/YYYYMMDD-HHMMSS-<slug>.md` before opening a
pull request. The `prompt-log` job in `.github/workflows/ci.yml` fails the
PR otherwise.

This is a near-guarantee, not a suggestion. Treat it the same as "tests
must pass."

## How to do it correctly

1. **Read** `prompts/PROMPTLOG.md` — the format spec, with full examples.
2. **Use the skill** at `.claude/skills/promptlog.md` — it's a step-by-step
   walkthrough. Invoke it as `/promptlog` if your harness supports
   skill invocation; otherwise read the file directly and follow the steps.
3. **Sanitize**: redact secrets, internal URLs, customer data, PII.
   The test is "would you be comfortable if this appeared on the front
   page of Hacker News?"
4. **Commit the prompt log file in the same commit as the code change**,
   or as an immediate follow-up commit. Either is fine — CI re-runs on
   every push.

## What if I'm a worktree agent that can't commit?

Then the orchestrator (the conversation that spawned you) is responsible
for committing the prompt log on your behalf, in a follow-up commit on
your branch. Surface in your final message: "I changed `src/foo.rs` —
the orchestrator should add a prompt log entry covering these decisions:
[bullet list of the key decisions]." The orchestrator will use the
`promptlog` skill.

If you _can_ commit, write the prompt log yourself.

## Local pre-commit hook (optional but recommended)

Install once per clone:

```sh
./scripts/install-hooks.sh
```

This installs `.git/hooks/pre-commit`, which mirrors the CI gate. To
bypass for a single intentional commit (e.g. worktree-agent code that
gets a follow-up prompt log commit):

```sh
SKIP_PROMPT_LOG=1 git commit -m "..."
```

Do not routinely skip the hook.

## Project facts

- **Language**: Rust, edition 2021.
- **Binary**: `isomage` — a CLI for browsing and extracting ISO 9660 / UDF
  images in userspace (no mount, no FUSE, no root).
- **Layout**:
  - `src/lib.rs` — public API and CLI argument plumbing
  - `src/main.rs` — `main()` entry point
  - `src/iso9660.rs` — ISO 9660 (with Joliet, Rock Ridge) parser
  - `src/udf.rs` — UDF parser (incl. metadata partitions, multi-extent files)
  - `src/tree.rs` — tree rendering for list mode
- **Tests** live as `#[cfg(test)] mod tests` inline in each `src/*.rs`
  module. There is no top-level `tests/` directory. Generated test ISOs
  live under `test_data/` and are produced by `make test-data`.
- **Build**: `cargo build` for debug, `make all` for cross-platform release.
- **CI**: `.github/workflows/ci.yml` runs build, test, and the prompt-log
  check on every PR.

## House style

- Match the surrounding code: comment density, naming, idiom. The
  parsers in `iso9660.rs` and `udf.rs` favor explicit byte offsets and
  inline comments citing the spec section — keep that style when extending
  them.
- Diagnostic output (verbose, progress, errors) goes to **stderr**. Only
  file data goes to stdout. Don't break that contract — `isomage -c` is
  used in pipelines.
- Read-only by design. Don't add ISO-writing features without an open issue
  and a prompt-log entry explaining the decision.
