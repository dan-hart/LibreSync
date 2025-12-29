#!/usr/bin/env bash
set -euo pipefail

REPO_PATH="${1:-.}"
FORCE=false

for arg in "$@"; do
  if [[ "$arg" == "--force" ]]; then
    FORCE=true
  fi
 done

cd "$REPO_PATH"

if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "Error: not a git repository: $REPO_PATH" >&2
  exit 1
fi

HOOK_DIR="$(git rev-parse --git-path hooks)"
HOOK_FILE="${HOOK_DIR}/pre-commit"

if [[ -e "$HOOK_FILE" && "$FORCE" == false ]]; then
  echo "Pre-commit hook already exists: $HOOK_FILE" >&2
  echo "Re-run with --force to back it up and replace." >&2
  exit 1
fi

if [[ -e "$HOOK_FILE" ]]; then
  BACKUP_FILE="${HOOK_FILE}.backup.$(date +%Y%m%d-%H%M%S)"
  cp "$HOOK_FILE" "$BACKUP_FILE"
  echo "Backed up existing hook to: $BACKUP_FILE"
fi

cat <<'HOOK' > "$HOOK_FILE"
#!/usr/bin/env bash
set -euo pipefail

# ASP preflight + git-secrets scan
if command -v git-secrets >/dev/null 2>&1; then
  git secrets --scan --cached
fi

if command -v asp-preflight >/dev/null 2>&1; then
  asp-preflight --staged --strict
elif [[ -x "./scripts/utilities/asp-preflight.sh" ]]; then
  ./scripts/utilities/asp-preflight.sh --staged --strict
else
  echo "ASP preflight script not found. Install or fix path." >&2
  exit 1
fi
HOOK

chmod +x "$HOOK_FILE"

echo "Installed ASP pre-commit hook: $HOOK_FILE"
