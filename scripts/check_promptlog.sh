#!/usr/bin/env bash
# Check that a diff touching code-tracked paths also adds a file under prompts/.
#
# The watched paths (see WATCH_REGEX below) are anything under src/, tests/,
# or scripts/. This is the single source of truth — update it here if the
# repo grows new code directories.
#
# Usage:
#   scripts/check_promptlog.sh <base> [<head>]      # diff-range mode
#                                                   # base/head may be refs or SHAs
#   scripts/check_promptlog.sh --staged             # pre-commit mode
#
# Exit codes:
#   0 — no prompt log required, or one was added
#   1 — source changed but no prompts/ file was added
#   2 — usage error, or a passed ref/SHA cannot be resolved

set -euo pipefail

WATCH_REGEX='^(src/|tests/|scripts/)'

usage() {
  echo "usage: $0 <base> [<head>]    # diff-range mode (refs or SHAs)" >&2
  echo "       $0 --staged           # check staged files (pre-commit)" >&2
  exit 2
}

# Run `git diff` and a filter pipeline, suppressing the no-match exit
# from `grep` (so an empty result is "" not an error) WITHOUT suppressing
# real failures from `git diff` itself (a missing ref must not be silently
# treated as "no changes" — that would let CI bypass the gate).
diff_filtered() {
  # $@ — the rest of the `git diff` arguments
  local out
  out=$(git diff "$@") || return $?
  # grep -E exits 1 on no match; that's expected, so swallow only that.
  printf '%s\n' "$out" | { grep -E "$WATCH_REGEX" || true; }
}

added_prompts() {
  local out
  out=$(git diff "$@") || return $?
  printf '%s\n' "$out" | { grep '^prompts/' || true; } | grep -v '^prompts/PROMPTLOG\.md$' || true
}

if [ $# -lt 1 ]; then
  usage
fi

if [ "${1:-}" = "--staged" ]; then
  CODE=$(diff_filtered --cached --name-only --diff-filter=ACM)
  PROMPTS=$(added_prompts --cached --name-only --diff-filter=A)
  CONTEXT="staged changes"
else
  BASE="$1"
  HEAD="${2:-HEAD}"
  # Resolve both endpoints up front so we fail loudly (exit 2) rather than
  # silently treating a missing ref as an empty diff.
  for ref in "$BASE" "$HEAD"; do
    if ! git rev-parse --verify --quiet "$ref^{commit}" >/dev/null; then
      echo "error: cannot resolve git ref '$ref'" >&2
      echo "       (in CI, ensure actions/checkout uses fetch-depth: 0 or pass a SHA)" >&2
      exit 2
    fi
  done
  CODE=$(diff_filtered --name-only --diff-filter=ACM "$BASE...$HEAD")
  PROMPTS=$(added_prompts --name-only --diff-filter=A "$BASE...$HEAD")
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
