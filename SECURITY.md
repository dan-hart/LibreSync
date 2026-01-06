# Security Policy

LibreSync is designed for **local-only, device-to-device synchronization** with strong privacy and security defaults. The research plan emphasizes mutual authentication, encrypted transport, explicit device trust, and minimal metadata leakage.

## Security Principles
- **No cloud dependency**: connections are direct between trusted devices on LAN or a private overlay.
- **Mutual authentication**: devices must explicitly approve each other before syncing.
- **Encrypted transport**: all data in transit must be encrypted.
- **End-to-end encryption (E2EE) by default**: payloads are encrypted by the engine and not optional for consumers.
- **Least leakage**: discovery payloads should be minimal; metadata should be reduced where possible.

## Threat Model (Baseline)
- An attacker on the same local network attempting to eavesdrop or spoof devices.
- A malicious device attempting to join a trusted cluster.
- Replay attacks and message tampering.
- Stolen device with access to local state files.
- Device discovery metadata leakage on shared networks.

## Mitigations (current + planned)
- TLS with device keys for transport security.
- E2EE for payloads and encrypted local state/backups.
- Explicit linking and per-app allowlists.
- Minimal discovery payloads and opt-out discovery flags.
- App-key rotation and device-key rotation (CLI) with re-encryption/rotation guidance.
- Planned: key export/import for recovery and assisted revocation workflows.

## Privacy notes
- No cloud dependencies or third-party relays in the default flow.
- Discovery is limited to LAN or explicitly configured private overlays.
- Metadata is minimized; device IDs and app IDs are the primary discovery fields.

## E2EE status and threat-model impact
- Sync payloads are now encrypted by the engine with a shared app-level key.
- E2EE is mandatory for consumers; there is no opt-out path.
- Engine state files and backups are encrypted at rest using the same app-level key.
- App-key rotation is implemented (state/backups are re-encrypted).
- Device-key rotation is implemented (new fingerprints require re-linking).
- Assisted re-keying and export/import flows are still pending.

## Key model (current)
- **Device keys**: per-device TLS keys used for transport security and identity fingerprints.
- **App key**: shared app-level key used for payload encryption and encrypted state/backup storage.
- The app key is never optional; the engine always encrypts payloads and local state.

## Linking and key exchange (current)
- Linking occurs only between devices with the same app ID.
- The app key is exchanged during linking inside the TLS channel.
- If devices disagree on the app key, the newest linking wins and the local device adopts the remote app key.
- Linking always requires explicit consent (or explicit auto-accept configuration).

## App-key lifecycle (current + planned)
**Current behavior**
- The app-level key is generated on first init and stored locally per app config.
- Linking exchanges app keys; the local device adopts the remote app key when they differ.
- The app-level key encrypts sync payloads and state/backup storage. It is never optional.
- `libresync key rotate` generates a new app key and re-encrypts state/backups. The allowlist is cleared by default to force re-linking.
- `libresync key export-app` and `libresync key import-app` support key portability and recovery.

**Planned behavior**
- Support guided re-keying on trust changes (e.g., removing a device).

## Key rotation behavior
- **Trigger conditions**: manual rotation, device removal, suspected compromise, or periodic hygiene.
- **App-key rotation** (`libresync key rotate`):
  1) Generates a new app key.
  2) Re-encrypts local state and backups.
  3) Clears the allowlist by default to force re-linking (optional `--keep-allowlist`).
- **Device-key rotation** (`libresync key rotate-device`):
  1) Generates new TLS keys and a new fingerprint.
  2) Requires remote devices to re-link to trust the new fingerprint.
  3) Optionally clears the allowlist to force re-linking.
- **Key export/import**:
  - `libresync key export-app` / `libresync key import-app` for app-level key portability.
  - `libresync key export-device` / `libresync key import-device` for device TLS keys.

## Re-linking and revocation
- `libresync unlink` removes a device from the allowlist; it must re-link before any refresh.
- App-key rotation clears the allowlist by default, forcing re-linking on all devices.
- Device-key rotation changes the local fingerprint; remote devices must re-link to trust it.

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
