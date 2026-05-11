#!/usr/bin/env bash
# Check that a diff touching src/ also adds a file under prompts/.
#
# Usage:
#   scripts/check_promptlog.sh <base-ref> [<head-ref>]
#
# Examples:
#   scripts/check_promptlog.sh origin/main HEAD          # PR-style check
#   scripts/check_promptlog.sh --staged                  # pre-commit-style check
#
# Exit codes:
#   0 — no prompt log required, or one was added
#   1 — source changed but no prompts/ file was added
#   2 — usage error

set -euo pipefail

# Paths whose changes require a prompt log entry. Adjust here if the repo
# grows new code directories (e.g. a top-level `tests/` directory).
WATCH_REGEX='^(src/|Cargo\.toml$)'

usage() {
  echo "usage: $0 <base-ref> [<head-ref>]   # diff-range mode" >&2
  echo "       $0 --staged                  # check staged files (pre-commit)" >&2
  exit 2
}

if [ $# -lt 1 ]; then
  usage
fi

if [ "${1:-}" = "--staged" ]; then
  CODE=$(git diff --cached --name-only --diff-filter=ACM | grep -E "$WATCH_REGEX" || true)
  PROMPTS=$(git diff --cached --name-only --diff-filter=A | grep '^prompts/' | grep -v '^prompts/PROMPTLOG\.md$' || true)
  CONTEXT="staged changes"
else
  BASE="$1"
  HEAD="${2:-HEAD}"
  CODE=$(git diff --name-only --diff-filter=ACM "$BASE...$HEAD" | grep -E "$WATCH_REGEX" || true)
  PROMPTS=$(git diff --name-only --diff-filter=A "$BASE...$HEAD" | grep '^prompts/' | grep -v '^prompts/PROMPTLOG\.md$' || true)
  CONTEXT="$BASE...$HEAD"
fi

if [ -z "$CODE" ]; then
  echo "✓ No source changes in $CONTEXT — prompt log not required."
  exit 0
fi

if [ -z "$PROMPTS" ]; then
  cat >&2 <<EOF
✗ Source code changed in $CONTEXT but no prompt log was added.

  Add a file at prompts/YYYYMMDD-HHMMSS-<slug>.md describing the human
  prompts and key assistant decisions in this change. See:

    prompts/PROMPTLOG.md            (format spec)
    .claude/skills/promptlog.md     (step-by-step skill for agents)

  Changed source files:
$(echo "$CODE" | sed 's/^/    /')

  To bypass for a single legitimate commit (e.g. worktree-agent code that
  gets a prompt log in the next commit):

    SKIP_PROMPT_LOG=1 git commit ...

EOF
  exit 1
fi

echo "✓ Prompt log present in $CONTEXT:"
echo "$PROMPTS" | sed 's/^/    /'
exit 0
