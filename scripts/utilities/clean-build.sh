#!/usr/bin/env bash
set -euo pipefail

# Build with a clean environment (avoid leaking credentials into build caches).
SENSITIVE_VARS=(
  OPENAI_API_KEY
  GPR_API_KEY
  JIRA_API_TOKEN
  GITHUB_TOKEN
  DATABASE_URL
  AWS_ACCESS_KEY_ID
  AWS_SECRET_ACCESS_KEY
)

for var in "${SENSITIVE_VARS[@]}"; do
  unset "$var"
done

echo "Clean environment prepared. Add your build command below."
# Example:
# cargo build
