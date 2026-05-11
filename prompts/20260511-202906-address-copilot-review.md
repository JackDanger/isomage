---
session: "introduce-promptlog"
timestamp: "2026-05-11T20:29:06Z"
model: claude-opus-4-7
sequence: 2
pr: 6
---

## Human

(Continuation of session — see sequence 1 at
`prompts/20260511-201219-introduce-promptlog-pattern.md` for the original
prompt chain.)

Address Copilot's review feedback on PR #6.

## Assistant

Copilot left ten inline comments. I sorted them into real bugs, false
positives, and doc-alignment nits, and acted on each.

### Real bugs — fixed

**`.claude/` was in `.gitignore`, so `.claude/skills/promptlog.md` was
never committed.** Five other files (CLAUDE.md, CONTRIBUTING.md,
README.md, prompts/PROMPTLOG.md, scripts/check_promptlog.sh) referenced
that path. Copilot flagged five dead links — all the same root cause.
Narrowed `.gitignore` to `.claude/worktrees/` and
`.claude/settings.local.json` so the skill (and any future tracked
skills) survives. Added a comment in `.gitignore` explaining the split.

**`check_promptlog.sh` masked `git diff` errors with `|| true`.** If
the base ref didn't resolve, the script silently printed "No source
changes" and exited 0, bypassing the gate. Two changes:

1. In diff-range mode, resolve both endpoints with
   `git rev-parse --verify --quiet "$ref^{commit}"` and exit 2 on
   failure with a message telling the operator what's likely wrong
   (missing `fetch-depth: 0`).
2. Refactored the diff-then-grep pipelines into helper functions
   `diff_filtered` / `added_prompts` so `git diff` failure propagates
   while `grep`'s "no match → exit 1" stays swallowed. Previously a
   single `||true` covered both, which was the bug.

**`ci.yml` passed `origin/${{ github.base_ref }}` (a ref) to the
script.** Even with `fetch-depth: 0`, depending on the checkout's
`refspec` the remote-tracking ref isn't guaranteed to exist. Switched
to passing `${{ github.event.pull_request.base.sha }}` and
`…head.sha` — those are SHAs straight from the PR event payload and
are always present in the local clone after the full fetch.

**`install-hooks.sh` used `$repo_root/.git/hooks`, which is wrong in
worktrees.** In a Git worktree `.git` is a regular file pointing at
the main gitdir, and the real hooks directory lives elsewhere. Switched
to `git rev-parse --git-path hooks`, which returns the correct path in
both ordinary clones and worktrees. (Verified locally — this very
worktree returns `/Users/.../isomage/.git/hooks`, the main repo's
hooks dir, which is exactly where Git expects hooks for the worktree.)

**`install-hooks.sh` unconditionally clobbered an existing
pre-commit hook.** Added a check that recognizes our own hook (via a
sentinel string `isomage pre-commit hook`) and a back-up step for any
other existing hook (`pre-commit.pre-isomage.<timestamp>.bak`). An
agent or contributor who already had a hook installed gets it
preserved.

### Doc alignment — fixed

`prompts/PROMPTLOG.md` said "If your changes touch any file under
`src/`" and "The PR adds, changes, or modifies any file under `src/`",
but the gate also watches `Cargo.toml`. Updated both lines to name
both paths. Also added a pointer to `scripts/check_promptlog.sh`
`WATCH_REGEX` as the single source of truth — so the next time the
watch set changes, the docs don't have to.

`scripts/check_promptlog.sh` header comment said "diffs touching
`src/`" but actually watched `src/` + `Cargo.toml`. Rewrote the
header to describe the watched set in prose and point at `WATCH_REGEX`
as the authoritative answer.

### False positives — confirmed and dismissed

The five dead-link comments turned into one root cause (the
`.gitignore` issue above). Once the skill file is committed, those
links all resolve, so no individual file needed text changes — only
`.gitignore` and adding the file did.

### What I skipped

- Adding a `pre-receive` hook on the server side. The CI gate already
  covers the server side; a `pre-receive` would only help repos that
  bypass CI. Out of scope for now.
- Replacing the `set -euo pipefail` style. Copilot didn't ask for it
  and the scripts are short — micro-style.

### Verified locally

- `bash -n` on both scripts: clean.
- `scripts/check_promptlog.sh refs/heads/no-such-branch HEAD` →
  exit 2 with the new error message. (Previously: exit 0, silently
  bypassing.)
- `git rev-parse --git-path hooks` from inside this worktree →
  `/Users/jackdanger/www/isomage/.git/hooks`, which is the right place.
