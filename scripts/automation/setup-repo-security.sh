#!/usr/bin/env bash
set -euo pipefail

REPO_PATH="${1:-.}"

cd "$REPO_PATH"

if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "Error: not a git repository: $REPO_PATH" >&2
  exit 1
fi

if ! command -v git-secrets >/dev/null 2>&1; then
  echo "Error: git-secrets not installed. Install with: brew install git-secrets" >&2
  exit 1
fi

# Install git-secrets hooks (ignore if already present)
if ! git secrets --install >/dev/null 2>&1; then
  echo "git-secrets hooks already present or install failed; continuing." >&2
fi

git secrets --register-aws

# Add curated patterns (avoid overly broad matches)
add_pattern() {
  local pattern="$1"
  local literal="${2:-false}"
  if git config --get-all secrets.patterns | grep -Fq -- "$pattern" >/dev/null 2>&1; then
    return 0
  fi
  if [[ "$literal" == "true" || "$pattern" == -* ]]; then
    git secrets --add -l -- "$pattern"
  else
    git secrets --add "$pattern"
  fi
}

add_pattern 'sk-[A-Za-z0-9]{20,}'
add_pattern 'sk-proj-[A-Za-z0-9_-]{20,}'
add_pattern 'ghp_[A-Za-z0-9]{36}'
add_pattern 'gho_[A-Za-z0-9]{36}'
add_pattern 'ghs_[A-Za-z0-9]{36}'
add_pattern 'github_pat_[A-Za-z0-9_]{70,}'
add_pattern 'ATATT[A-Za-z0-9_-]{10,}'
add_pattern 'AIza[0-9A-Za-z_-]{35}'
add_pattern 'xox[baprs]-[A-Za-z0-9-]{10,}'
add_pattern 'BEGIN[[:space:]]+(RSA|DSA|EC|OPENSSH)[[:space:]]+PRIVATE[[:space:]]+KEY'
add_pattern 'BEGIN[[:space:]]+PGP[[:space:]]+PRIVATE[[:space:]]+KEY[[:space:]]+BLOCK'
add_pattern 'Bearer[[:space:]]+[A-Za-z0-9._-]{20,}'
add_pattern '[Aa][Pp][Ii][_-]?[Kk][Ee][Yy][[:space:]]*[:=][[:space:]]*[^[:space:]]+'
add_pattern '[Pp][Aa][Ss][Ss][Ww][Oo][Rr][Dd][[:space:]]*[:=][[:space:]]*[^[:space:]]+'
add_pattern 'postgres://[^:]+:[^@]+@'
add_pattern 'mysql://[^:]+:[^@]+@'

# Install ASP pre-commit hook (backup existing)
./scripts/automation/install-asp-hooks.sh . --force

echo "Repository security configured."
