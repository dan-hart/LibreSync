#!/usr/bin/env bash
set -euo pipefail

if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "Error: not a git repository" >&2
  exit 1
fi

echo "Security audit: checking tracked files for sensitive patterns..."

# 1) Block tracked sensitive file types
TRACKED_SENSITIVE=$(git ls-files | grep -E '(\.env$|\.env\.|\.pem$|\.key$|\.p12$|\.pfx$|\.keystore$|\.kdbx$|credentials|secret|token|password)' || true)
if [[ -n "$TRACKED_SENSITIVE" ]]; then
  echo "Error: tracked files with sensitive names detected:" >&2
  echo "$TRACKED_SENSITIVE" >&2
  exit 1
fi

# 2) Run git-secrets if available
if command -v git-secrets >/dev/null 2>&1; then
  SCAN_FILES=()
  while IFS= read -r file; do
    [[ "$file" == ".gitallowed" ]] && continue
    SCAN_FILES+=("$file")
  done < <(git ls-files || true)
  if [[ ${#SCAN_FILES[@]} -gt 0 ]]; then
    git secrets --scan "${SCAN_FILES[@]}"
  else
    git secrets --scan
  fi
  git secrets --scan-history
  echo "git-secrets scan: OK"
else
  echo "Warning: git-secrets not installed; running fallback scan." >&2
  PATTERN='(sk-proj-[A-Za-z0-9_-]{20,}|sk-[A-Za-z0-9]{20,}|ghp_[A-Za-z0-9]{36}|gho_[A-Za-z0-9]{36}|github_pat_[A-Za-z0-9_]{70,}|ATATT[A-Za-z0-9_-]{10,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{35}|xox[baprs]-[A-Za-z0-9-]{10,}|-----BEGIN (RSA|DSA|EC|OPENSSH) PRIVATE KEY-----|Bearer [A-Za-z0-9._-]{20,})'
  SCAN_FILES=()
  while IFS= read -r file; do
    [[ "$file" == ".gitallowed" ]] && continue
    SCAN_FILES+=("$file")
  done < <(git ls-files || true)

  MATCHES=""
  if [[ ${#SCAN_FILES[@]} -gt 0 ]]; then
    # rg/grep return 1 when no lines match; that is a clean audit result.
    # Capture output explicitly so only real findings fail the fallback audit.
    set +e
    if command -v rg >/dev/null 2>&1; then
      MATCHES=$(rg -n -e "$PATTERN" -- "${SCAN_FILES[@]}")
    else
      MATCHES=$(grep -n -E "$PATTERN" -- "${SCAN_FILES[@]}")
    fi
    STATUS=$?
    set -e

    if [[ $STATUS -gt 1 ]]; then
      echo "Error: fallback secret scan failed" >&2
      exit "$STATUS"
    fi
  fi

  if [[ -n "$MATCHES" ]]; then
    echo "$MATCHES" >&2
    exit 1
  fi
fi

echo "Security audit: OK"
