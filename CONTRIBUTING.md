# Contributing

Thanks for helping with LibreSync. This project is security- and privacy-first. Contributions should favor safety and data preservation over speed.

## Getting Started
- The repo is currently documentation-focused. Please keep changes concise and specific.
- If you add build tooling later, ensure security checks are in place before first build or commit.

## Security & Privacy Checks (Required)
- Install and configure git-secrets before committing:
  - `brew install git-secrets`
  - `git secrets --install`
  - `git secrets --register-aws`
- Or run the one-shot setup: `./scripts/automation/setup-repo-security.sh .`
- Install the ASP pre-commit hook:
  - `./scripts/automation/install-asp-hooks.sh .`
- Run the audit script before pushing:
  - `./scripts/utilities/security-audit.sh`
- If git-secrets flags literal pattern examples in scripts, add a narrow allow rule to `.gitallowed`.

## Sensitive Data Rules
- Never commit `.env`, private keys (`*.pem`, `*.key`, `*.p12`), or credentials.
- Avoid real names, emails, phone numbers, addresses, or other PII in docs or code.
- Use placeholders like `user@example.com` and `YOUR_API_KEY_HERE`.

## Data Preservation
- If you introduce scripts that modify user data, they must include a dry-run mode and sample-data testing before execution.
- Never change data storage paths without a migration plan and explicit approval.

## Pull Requests
- Keep PRs focused and explain the security/privacy impact (or lack thereof).
- If you add new build artifacts, update `.gitignore` before committing.

## Releases
- Keep crate manifests, the LibreSyncAlwaysOn app version, and `RELEASES.md` aligned.
- Run `./scripts/utilities/check-release-readiness.sh` before cutting a release.
- Follow `docs/RELEASING.md` for the full release checklist, including coverage, smoke build, clean-tree verification, and `vX.Y.Z` tag creation.

## Questions
If unsure about a change that could impact security, privacy, or data integrity, open an issue or start a discussion before proceeding.
