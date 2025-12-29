#!/usr/bin/env bash
set -euo pipefail

MODE="staged"
STRICT=false
ACK_DATA_PATH=false
ACK_SYSTEM=false
ACK_DISPLAY=false
ACK_PRIVACY=false

usage() {
  cat << 'USAGE'
ASP Preflight - AI Safety Policy checks for commits and risky changes

Usage:
  asp-preflight.sh [--staged|--all] [--strict]
                  [--ack-data-path] [--ack-system] [--ack-display] [--ack-privacy]

Options:
  --staged           Scan staged changes (default)
  --all              Scan unstaged working tree changes
  --strict           Fail if high-risk changes lack explicit ack
  --ack-data-path    Acknowledge data path/storage changes
  --ack-system       Acknowledge system/boot changes
  --ack-display      Acknowledge display/graphics changes
  --ack-privacy      Acknowledge potential PII/privacy hits
  -h, --help         Show this help

Examples:
  ./scripts/utilities/asp-preflight.sh --staged --strict
  ./scripts/utilities/asp-preflight.sh --staged --strict --ack-data-path
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --staged)
      MODE="staged"
      shift
      ;;
    --all)
      MODE="all"
      shift
      ;;
    --strict)
      STRICT=true
      shift
      ;;
    --ack-data-path)
      ACK_DATA_PATH=true
      shift
      ;;
    --ack-system)
      ACK_SYSTEM=true
      shift
      ;;
    --ack-display)
      ACK_DISPLAY=true
      shift
      ;;
    --ack-privacy)
      ACK_PRIVACY=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage
      exit 1
      ;;
  esac
 done

if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "Error: not a git repository" >&2
  exit 1
fi

DIFF_CMD=(git diff)
NAME_CMD=(git diff --name-only)
if [[ "$MODE" == "staged" ]]; then
  DIFF_CMD+=(--cached)
  NAME_CMD+=(--cached)
fi

CHANGED_FILES=()
while IFS= read -r line; do
  if [[ -n "$line" ]]; then
    CHANGED_FILES+=("$line")
  fi
 done < <("${NAME_CMD[@]}")

if [[ ${#CHANGED_FILES[@]} -eq 0 ]]; then
  echo "ASP preflight: no changes to scan."
  exit 0
fi

echo "ASP preflight: scanning ${#CHANGED_FILES[@]} file(s)..."

# 1) Block sensitive file types outright
SENSITIVE_FILES=()
for f in "${CHANGED_FILES[@]}"; do
  case "$f" in
    .env|.env.*|*.env|*.env.*)
      if [[ "$f" != *.example ]]; then
        SENSITIVE_FILES+=("$f")
      fi
      ;;
    *.pem|*.key|*.p12|*.pfx|*.keystore|*.kdbx)
      SENSITIVE_FILES+=("$f")
      ;;
    *credentials.json|*secrets.json|*secret.json|*private.key|*id_rsa*|*id_ed25519*)
      SENSITIVE_FILES+=("$f")
      ;;
    .lock-waf*|.waf-*)
      SENSITIVE_FILES+=("$f")
      ;;
  esac
 done

if [[ ${#SENSITIVE_FILES[@]} -gt 0 ]]; then
  echo "Error: sensitive files detected in changes:" >&2
  printf '  - %s\n' "${SENSITIVE_FILES[@]}" >&2
  echo "Remove these from changes or add safe placeholders only." >&2
  exit 1
fi

# 2) Warning for filename keywords (non-blocking)
KEYWORD_WARNINGS=()
for f in "${CHANGED_FILES[@]}"; do
  if [[ "$f" =~ [Pp]rivate|[Ss]ecret|[Ss]ensitive|[Cc]redential|[Tt]oken|[Pp]assword ]]; then
    KEYWORD_WARNINGS+=("$f")
  fi
 done

if [[ ${#KEYWORD_WARNINGS[@]} -gt 0 ]]; then
  echo "Warning: filenames contain sensitive keywords (review carefully):" >&2
  printf '  - %s\n' "${KEYWORD_WARNINGS[@]}" >&2
fi

# 3) Secrets scan (prefer git-secrets)
if command -v git-secrets >/dev/null 2>&1; then
  GIT_SECRETS_OUTPUT=""
  if GIT_SECRETS_OUTPUT=$(git secrets --scan --cached 2>&1); then
    echo "git-secrets scan: OK"
  else
    if echo "$GIT_SECRETS_OUTPUT" | grep -qiE 'unknown option|usage'; then
      if git secrets --scan >/dev/null 2>&1; then
        echo "git-secrets scan: OK (fallback)"
      else
        echo "Error: git-secrets scan failed. Review repository." >&2
        git secrets --scan || true
        exit 1
      fi
    else
      echo "Error: git-secrets scan failed. Review staged changes." >&2
      echo "$GIT_SECRETS_OUTPUT" >&2
      exit 1
    fi
  fi
else
  echo "Warning: git-secrets not installed; running fallback scan." >&2
  if "${DIFF_CMD[@]}" | grep -nE \
    '(sk-proj-[A-Za-z0-9_-]{20,}|sk-[A-Za-z0-9]{20,}|ghp_[A-Za-z0-9]{36}|gho_[A-Za-z0-9]{36}|github_pat_[A-Za-z0-9_]{70,}|ATATT[A-Za-z0-9_-]{10,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{35}|xox[baprs]-[A-Za-z0-9-]{10,}|-----BEGIN (RSA|DSA|EC|OPENSSH) PRIVATE KEY-----|Bearer [A-Za-z0-9._-]{20,})' \
    >/dev/null; then
    echo "Error: potential secret detected in diff." >&2
    "${DIFF_CMD[@]}" | grep -nE \
      '(sk-proj-[A-Za-z0-9_-]{20,}|sk-[A-Za-z0-9]{20,}|ghp_[A-Za-z0-9]{36}|gho_[A-Za-z0-9]{36}|github_pat_[A-Za-z0-9_]{70,}|ATATT[A-Za-z0-9_-]{10,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{35}|xox[baprs]-[A-Za-z0-9-]{10,}|-----BEGIN (RSA|DSA|EC|OPENSSH) PRIVATE KEY-----|Bearer [A-Za-z0-9._-]{20,})' \
      || true
    exit 1
  fi
fi

# 4) Privacy/PII checks on added lines
PRIVACY_HIT=false
DIFF_LINES=$({ "${DIFF_CMD[@]}" | grep -E '^\+' | grep -vE '^\+\+\+'; } || true)

EMAIL_HITS=$(echo "$DIFF_LINES" | grep -nE '[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}' | \
  grep -vE '@(example\.com|example\.org|example\.net)' || true)
PHONE_HITS=$(echo "$DIFF_LINES" | grep -nE '\b(\+?1[-. ]?)?(\(?[0-9]{3}\)?[-. ]?)[0-9]{3}[-. ]?[0-9]{4}\b' || true)
SSN_HITS=$(echo "$DIFF_LINES" | grep -nE '\b[0-9]{3}-[0-9]{2}-[0-9]{4}\b' || true)

if [[ -n "$EMAIL_HITS" || -n "$PHONE_HITS" || -n "$SSN_HITS" ]]; then
  PRIVACY_HIT=true
  echo "Notice: potential PII detected in added lines (review carefully)." >&2
  if [[ -n "$EMAIL_HITS" ]]; then
    echo "Email-like entries (non-example domains):" >&2
    echo "$EMAIL_HITS" >&2
  fi
  if [[ -n "$PHONE_HITS" ]]; then
    echo "Phone-like entries:" >&2
    echo "$PHONE_HITS" >&2
  fi
  if [[ -n "$SSN_HITS" ]]; then
    echo "SSN-like entries:" >&2
    echo "$SSN_HITS" >&2
  fi

  if [[ "$STRICT" == true && "$ACK_PRIVACY" == false ]]; then
    echo "Error: acknowledge with --ack-privacy to proceed." >&2
    exit 2
  fi
fi

# 5) Data/system/display checks (skip docs to reduce false positives)
DATA_PATH_HIT=false
SYSTEM_HIT=false
DISPLAY_HIT=false

is_low_risk_file() {
  case "$1" in
    *.md|*.txt|LICENSE|LICENSE.*|COPYING|COPYING.*|NOTICE|NOTICE.*|.gitignore|.gitallowed|scripts/utilities/asp-preflight.sh|scripts/utilities/security-audit.sh|scripts/automation/setup-repo-security.sh)
      return 0
      ;;
  esac
  return 1
}

for f in "${CHANGED_FILES[@]}"; do
  if is_low_risk_file "$f"; then
    continue
  fi

  FILE_DIFF=$({ "${DIFF_CMD[@]}" -- "$f"; } || true)
  if [[ -z "$FILE_DIFF" ]]; then
    continue
  fi

  FILE_LINES=$(echo "$FILE_DIFF" | grep -E '^[+-]' | grep -vE '^(\+\+\+|---)' || true)
  if [[ -z "$FILE_LINES" ]]; then
    continue
  fi

  if echo "$FILE_LINES" | grep -nE '(DB_PATH|DATA_DIR|STORAGE_PATH|APP_DATA_DIR|Application Support|database\.db|\.db"|\.db\x27)' >/dev/null; then
    DATA_PATH_HIT=true
  fi

  if echo "$FILE_LINES" | grep -nE '(GRUB_CMDLINE|/etc/default/grub|grub2-mkconfig|grubby|fstab|initramfs|dracut|mkinitcpio|sysctl|kernel\.|systemctl|dnf |apt |nixos-rebuild)' >/dev/null; then
    SYSTEM_HIT=true
  fi

  if echo "$FILE_LINES" | grep -nE '(gdm|sddm|xorg|wayland|nvidia|nouveau|akmod-nvidia|display manager)' >/dev/null; then
    DISPLAY_HIT=true
  fi
done

if [[ "$DATA_PATH_HIT" == true ]]; then
  echo "Notice: data path/storage changes detected." >&2
  if [[ "$STRICT" == true && "$ACK_DATA_PATH" == false ]]; then
    echo "Error: acknowledge with --ack-data-path to proceed." >&2
    exit 2
  fi
fi

if [[ "$SYSTEM_HIT" == true ]]; then
  echo "Notice: system/boot changes detected." >&2
  if [[ "$STRICT" == true && "$ACK_SYSTEM" == false ]]; then
    echo "Error: acknowledge with --ack-system to proceed." >&2
    exit 2
  fi
fi

if [[ "$DISPLAY_HIT" == true ]]; then
  echo "Notice: display/graphics changes detected." >&2
  if [[ "$STRICT" == true && "$ACK_DISPLAY" == false ]]; then
    echo "Error: acknowledge with --ack-display to proceed." >&2
    exit 2
  fi
fi

echo "ASP preflight: OK"
