# Security Policy

LibreSync is designed for **local-only, device-to-device synchronization** with strong privacy and security defaults. The research plan emphasizes mutual authentication, encrypted transport, explicit device trust, and minimal metadata leakage.

## Security Principles
- **No cloud dependency**: connections are direct between trusted devices on LAN or a private overlay.
- **Mutual authentication**: devices must explicitly approve each other before syncing.
- **Encrypted transport**: all data in transit must be encrypted.
- **Least leakage**: discovery payloads should be minimal; metadata should be reduced where possible.

## Threat Model (Baseline)
- An attacker on the same local network attempting to eavesdrop or spoof peers.
- A malicious device attempting to join a trusted cluster.
- Replay attacks and message tampering.

## Repository Security Controls
- **.gitignore hardening**: sensitive files and build caches are excluded by default.
- **git-secrets (required)**: install hooks to block secrets before commit.
- **ASP preflight (required)**: `./scripts/utilities/asp-preflight.sh --staged --strict` blocks risky changes.
- **Security audit**: `./scripts/utilities/security-audit.sh` scans for secrets and history leaks.

## Build-System Safety
Build tools can serialize environment variables. Before running builds:
- Use `./scripts/utilities/clean-build.sh` to clear sensitive env vars.
- Inspect generated files before committing.
- Add new build artifacts to `.gitignore` first.

## Reporting Vulnerabilities
This repository does not yet have a private security contact. If you find a vulnerability:
1) Open a GitHub issue with **minimal details** and request a private channel.
2) Avoid sharing proof-of-concept exploits or sensitive data publicly.

## Incident Response (Summary)
If a secret or sensitive data is committed:
- **Stop and assess**: identify what was exposed and whether it was pushed.
- **Rotate credentials immediately** if exposed.
- **Remove from history** before pushing again.
- **Document** what happened and update safeguards.

## Security-Sensitive Changes
PRs affecting discovery, authentication, encryption, or key storage should include:
- A short security rationale.
- Notes on compatibility with the threat model in `RESEARCH.md`.
