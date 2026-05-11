---
name: promptlog
description: Write a sanitized prompt log entry for the current session into prompts/. Required before opening a PR that touches src/ — CI fails without it.
---

# Skill: Write a Prompt Log Entry

Use this skill whenever a session has changed `src/` or `Cargo.toml`. The
`prompt-log` job in `.github/workflows/ci.yml` fails any PR that touches
those paths without adding a file under `prompts/`.

**Always invoke this skill before pushing a PR.** Don't ask the user
whether to log — assume yes, and offer to skip only if they explicitly
said "don't log this."

## When to use

- Before committing code that touches `src/` (the optional local hook requires it)
- Before pushing a PR (CI requires it)
- At the end of a session
- Any time the user says "log the prompts" or "record this session"

## Process

1. **See what changed** in this session:

   ```bash
   git diff --name-only --diff-filter=ACM origin/main...HEAD
   git log --oneline origin/main..HEAD
   ```

   If nothing under `src/` or `Cargo.toml` was modified, the CI gate will
   skip — you don't need a prompt log. Stop here.

2. **Pick the timestamp and slug**:

   - Timestamp: current UTC time, formatted `YYYYMMDD-HHMMSS`
     ```bash
     date -u +'%Y%m%d-%H%M%S'
     ```
   - Slug: 2–5 lowercase url-safe words describing the session
     (e.g. `fix-udf-metadata-partition`, `add-rock-ridge-symlinks`)
   - File: `prompts/{timestamp}-{slug}.md`

3. **Write the file** using the required format:

   ```markdown
   ---
   session: "<short-id>"
   timestamp: "<ISO 8601 UTC>"
   model: claude-opus-4-7
   ---

   ## Human

   <Summarize what the user asked for in this session. Redact secrets, PII,
   internal URLs, customer data. Keep technical substance and intent. One
   paragraph per distinct request.>

   ## Assistant

   ### Key decisions

   **<Decision title>**: <Why this approach was chosen. What alternatives
   were considered and rejected, and why. Future reviewers need the
   reasoning, not a changelog — git diff shows what changed.>

   **<Another decision>**: <...>

   **What I skipped**: <Anything deliberately deferred, with the reason.>
   ```

4. **Stage the file** in the same commit as the code change, when possible:

   ```bash
   git add prompts/{filename}.md
   ```

   If the code is already committed (worktree-agent pattern), commit the
   prompt log as a follow-up — CI re-runs and passes on the next push.

## Rules

- **Session ID**: short unique string, reused for all files in the same
  conversation.
- **Timestamp**: when the conversation phase _started_, not when you're
  writing this — use a stable timestamp for the session.
- **Do not** summarize what files changed (that's what `git diff` is for).
- **Do** explain WHY — architectural choices, rejected alternatives, tradeoffs.
- **One YAML frontmatter block per file.** Never use a second `---`…`---`
  block mid-file.
- Keep `reconstructed: true` in frontmatter if writing retroactively.
- **Never rewrite or shorten existing prompt files** — they are permanent
  record. Append-only.
- For external contributor PRs: use `author` and `pr` frontmatter fields
  to give credit. Write the log on their behalf rather than blocking
  the PR.

## Sanitization checklist

Before saving, scan the file for:

- Secrets, tokens, API keys, passwords
- Private URLs, internal hostnames, IP addresses
- Personal names that aren't already public (replace with role: "a teammate")
- Customer names, account IDs, business metrics
- Large pasted logs or transcripts (summarize: `[Pasted: 80 lines of UDF hex dump]`)

The test: **would you be comfortable if this file appeared on the front
page of Hacker News?**

## Example

```markdown
---
session: "udf-meta-fix"
timestamp: "2026-05-11T20:00:00Z"
model: claude-opus-4-7
---

## Human

Blu-ray with metadata partitions panics in `read_extent` at udf.rs:412.
Fix it and add a regression test.

## Assistant

### Key decisions

**Root cause**: `read_extent` assumed every partition reference resolved
to a physical partition. Metadata partitions are virtual — they redirect
through a partition map. Single-redirect resolution is enough; the spec
forbids chained metadata partitions.

**Fix location**: Added partition-map resolution inside
`resolve_partition()` rather than at every call site. All extent reads
need the same resolution, and centralizing it keeps the code paths
honest.

**Test approach**: Generated a synthetic UDF image with `mkudffs` in the
Makefile rather than checking in a binary. Keeps the repo small and
makes the test reproducible across machines.

**What I skipped**: Did not add support for chained metadata partitions
(would require a cycle-detection pass). Per UDF spec they are not
allowed, and no real-world disc has triggered the case in three years
of issues.
```

That file would be saved as `prompts/20260511-200000-fix-udf-metadata-partition.md`.

## See also

- `prompts/PROMPTLOG.md` — the full spec, including the CI gate and
  worktree-agent two-commit pattern
- `scripts/check_promptlog.sh` — the script the CI gate runs
- `scripts/install-hooks.sh` — install the same check as a local pre-commit hook
