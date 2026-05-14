# Prompt Log

When you make a git commit that changes source code, also commit a sanitized
log of the prompts that led to it — both the human's and the assistant's.

If you are an AI agent reading this file: **the CI gate will fail your PR if
it touches `src/` or `Cargo.toml` without adding a file under `prompts/`.**
Read on, or invoke the `promptlog` skill at `.claude/skills/promptlog.md`.

## Why

AI-assisted code has provenance: someone asked for it, an AI reasoned about
it, decisions were made. The prompt log captures that provenance so future
contributors (human or AI) can understand not just _what_ the code does, but
_why_ it was written this way and _who_ (human or model) made each decision.

Human review capacity scales with human-generated content. A reviewer can
skim five paragraphs of prompts faster than they can audit 800 lines of
generated Rust. The prompt log is what a reviewer reads first.

## Format

One file per conversation phase at `prompts/{timestamp}-{slug}.md`. Timestamp
is UTC, formatted `YYYYMMDD-HHMMSS`. Slug is a few lowercase url-safe words
summarizing the phase.

Each file has **one YAML frontmatter block** with file-level metadata. The
body uses `## Human` and `## Assistant` headings to separate entries.

```
---
session: "a1b2c3"
timestamp: "2026-05-11T20:00:00Z"
model: claude-opus-4-7
---

## Human

The UDF parser is panicking on a Blu-ray with metadata partitions. Stack
trace points at `udf.rs:412` in `read_extent`. Fix it and add a regression
test using `test_data/test_udf_meta.iso`.

## Assistant

### Key decisions

**Root cause**: `read_extent` assumed every partition reference resolved to
a physical partition, but metadata partitions are virtual — they redirect
through a partition map. Dereferencing the redirect once is enough; the
spec doesn't allow chained metadata partitions.

**Fix**: Added partition-map resolution in `resolve_partition()` ahead of
the extent read, with a single-redirect guard. Chose this over patching
each call site because all extent reads need the same resolution.

**Test approach**: Generated a synthetic UDF image with `mkudffs` in
`Makefile` rather than checking in a binary. Keeps the repo small and
makes the test reproducible.
```

That file would be named `prompts/20260511-200000-fix-udf-metadata-partition.md`.

**Do NOT use multiple `---` frontmatter blocks in one file.** Standard
YAML/markdown parsers only recognize the first. A second `---`…`---` block
mid-file looks like file corruption.

### Frontmatter fields

**Required:**

- `session` — short id (hex or kebab-case) reused for all prompts in one conversation
- `timestamp` — ISO 8601 UTC when the conversation phase started

**Optional:**

- `model` — model that generated the assistant reasoning (e.g. `claude-opus-4-7`)
- `author` — GitHub username of the original contributor, when someone else writes the prompt log on their behalf during PR review. Gives credit where it's due.
- `pr` — PR number this prompt log accompanies (e.g. `pr: 12`). Cross-references.
- `tools` — list of tools/agents used (e.g. `tools: [worktree, web-fetch]`)
- `sequence` — integer ordering files within a session (1, 2, 3…) when a session spans multiple files
- `reconstructed` — set to `true` when logging retroactively from transcripts

## What to log

Log prompts that lead to code changes. Skip purely conversational messages.
If the user says "don't log this," don't.

Include your own reasoning as assistant entries when you make non-obvious
decisions — architectural choices, workaround strategies, why you chose one
approach over another. These entries are the most valuable part of the log
for future reviewers trying to understand _why_ the code looks the way it
does.

For autonomous work (long-running sessions where the human said "keep
going"), log the high-level plan and key decision points, not every tool
call. One assistant entry per logical phase of work is enough.

### What a good assistant entry looks like

A reviewer in six months doesn't need to know _what_ changed — `git diff`
tells them that. They need to know _why_. Write decisions, not a changelog.

**Good** — explains reasoning:

```
## Assistant

### Key decisions

**Chose streaming reads over mmap** because isomage targets large
Blu-ray images (50+ GB) where mmap exhausts virtual address space on
32-bit targets and triggers page-fault storms on cold reads. Sequential
`pread` with a 1 MB buffer matches the disc's natural sector layout.

**Path resolution**: Normalized leading slashes in the public API rather
than rejecting them. The CLI invites both forms (`/etc/hostname` and
`etc/hostname`) and silently accepting both costs nothing.

**What I skipped**: Did not add a `--follow-symlinks` flag — Rock Ridge
symlinks inside ISOs are rare and resolving them safely (no escapes)
needs more thought. Filed as a TODO in `lib.rs`.
```

**Avoid** — this is a changelog, not a prompt log:

```
## Assistant

- Updated lib.rs
- Fixed bug in udf.rs
- Added 3 test cases
- Bumped clap to 4.5
```

That belongs in a commit message. The prompt log is for the reasoning
behind those changes.

## External contributor PRs

When reviewing a PR from an external contributor who didn't include a
prompt log, the reviewer (human or AI) should write one on their behalf.
Use the `author` and `pr` frontmatter fields:

```
---
session: "pr-12-review"
timestamp: "2026-05-11T20:00:00Z"
model: claude-opus-4-7
author: "some-contributor"
pr: 12
---

## Human

(From PR #12 by @some-contributor)

ISO 9660 directory entries with the multi-extent flag (file flags bit 7)
aren't being concatenated, so files larger than 4 GB read as truncated.
Patch reads all extents in order.

## Assistant

### Key decisions

**Concat at read time, not parse time**: keeps the directory-entry
representation flat and avoids a second pass over the path table.
```

The goal is provenance, not gatekeeping. If someone contributes great code
without a prompt log, write one for them during review rather than blocking
the PR. Credit them as the author.

## Retroactive logging

When reconstructing a prompt log from session transcripts after the fact:

- Use the session transcript timestamps, not the current time
- Mark retroactive entries with `reconstructed: true` in the frontmatter
- It's fine to summarize — a retroactive log captures intent and provenance, not a verbatim transcript
- For long autonomous sessions, one assistant entry summarizing the work is better than reconstructing every step

## Sanitization

The prompt must be safe to persist in a public repo. Redact secrets, PII,
internal URLs, and customer data. Keep the intent, the technical substance,
and anything already public.

The test: **would you be comfortable if this file appeared on the front
page of Hacker News?**

When something is ambiguous — a colleague's name, an internal project
codename, a business metric — redact the specific identifier but keep the
context. "[A colleague] suggested we try mmap" is fine. Summarize large
pastes rather than including them verbatim: `[Pasted: 40 lines of UDF
volume-descriptor hex dump]`.

When in doubt, redact.

## Never compact or summarize existing prompt files

Prompt files are permanent historical record. Never rewrite, shorten,
merge, or "clean up" existing entries — not even to save tokens or tidy
the directory. The verbosity is the point. A reviewer six months from now
needs the actual words, not a digest.

This applies to autonomous cleanup passes too. If an agent is asked to
"tidy the repo," `prompts/` is off-limits. The only valid operations on
existing prompt files are:

- Adding optional frontmatter fields (e.g. backfilling `commits:`)
- Redacting newly-discovered secrets or PII (with a note in the file explaining what was redacted and why)

Everything else is append-only.

## Committing

Add the new `prompts/*.md` file to the same commit as the code changes
when possible. If a session produces multiple commits, each one includes
only the prompts that led to it.

If you are working in a worktree-agent context where the code was
committed first, a follow-up commit containing just the prompt log file
is acceptable — the CI gate re-runs on each push.

## Worktree agents and prompt logs

When using `isolation: "worktree"` agents, the agent cannot create prompt
log files (they don't commit). The orchestrating conversation must create
and push prompt logs to each worktree branch after the agent completes.

**Pattern**:

1. Agent completes work in worktree (code + tests, no prompt log)
2. Orchestrator commits and pushes the agent's changes
3. Orchestrator creates the prompt log file capturing the agent's task and key decisions
4. Orchestrator commits and pushes the prompt log as a follow-up commit

`SKIP_PROMPT_LOG=1 git commit` exists for this: the first commit (code)
bypasses the local hook, and the second commit (prompt log) satisfies it.
CI will pass on the second push.

## CI gate

`.github/workflows/ci.yml` runs a `prompt-log` job on every pull request.
It fails the PR when:

- The PR adds, changes, or modifies any file under `src/`, `tests/`, or `scripts/`, **and**
- The PR does not add a new file under `prompts/`

The exact watched paths are defined by `WATCH_REGEX` in
`scripts/check_promptlog.sh` — that is the single source of truth.

To bypass legitimately (e.g. a worktree-agent code commit that will get a
prompt log in a follow-up), push the follow-up commit — CI re-runs and
passes.

## Local pre-commit hook (optional)

If you want the same check before you push:

```sh
./scripts/install-hooks.sh
```

This installs `.git/hooks/pre-commit`, which runs the same check as CI.
Bypass for a single commit with:

```sh
SKIP_PROMPT_LOG=1 git commit -m "..."
```

**Do not routinely skip the hook.** It exists because prompt logs are easy
to forget during autonomous work. The two-commit pattern (code then prompt
log) is the intended workflow for worktree agents — not a workaround.
