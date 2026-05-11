#!/usr/bin/env bash
# Install the local pre-commit hook that mirrors the CI prompt-log check.
#
# Usage:
#   ./scripts/install-hooks.sh

set -euo pipefail

repo_root=$(git rev-parse --show-toplevel)
hooks_dir="$repo_root/.git/hooks"
mkdir -p "$hooks_dir"

cat > "$hooks_dir/pre-commit" <<'HOOK'
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

chmod +x "$hooks_dir/pre-commit"
echo "Installed $hooks_dir/pre-commit"
echo "Bypass for a single commit with: SKIP_PROMPT_LOG=1 git commit ..."
