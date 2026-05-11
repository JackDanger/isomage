#!/usr/bin/env bash
# Install the local pre-commit hook that mirrors the CI prompt-log check.
#
# Usage:
#   ./scripts/install-hooks.sh

set -euo pipefail

# Use `git rev-parse --git-path hooks` so this works in both ordinary
# clones and worktrees. In a worktree, `.git` is a file pointing at the
# main repo's gitdir, and `$repo_root/.git/hooks` would be invalid.
hooks_dir=$(git rev-parse --git-path hooks)
mkdir -p "$hooks_dir"

hook="$hooks_dir/pre-commit"

if [ -e "$hook" ] && ! grep -q 'isomage pre-commit hook' "$hook" 2>/dev/null; then
  # Existing hook from somewhere else — don't clobber it silently.
  backup="$hook.pre-isomage.$(date -u +%Y%m%dT%H%M%SZ).bak"
  mv "$hook" "$backup"
  echo "Existing pre-commit hook backed up to $backup"
fi

cat > "$hook" <<'HOOK'
#!/usr/bin/env bash
# isomage pre-commit hook: enforce the promptlog pattern locally.
# Bypass with: SKIP_PROMPT_LOG=1 git commit ...
set -euo pipefail

if [ "${SKIP_PROMPT_LOG:-0}" = "1" ]; then
  echo "⚠ SKIP_PROMPT_LOG=1 — skipping prompt log check (CI will re-check)." >&2
  exit 0
fi

repo_root=$(git rev-parse --show-toplevel)
"$repo_root/scripts/check_promptlog.sh" --staged
HOOK

chmod +x "$hook"
echo "Installed $hook"
echo "Bypass for a single commit with: SKIP_PROMPT_LOG=1 git commit ..."
