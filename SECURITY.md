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
- Mutual TLS (rustls 0.23) with self-signed device keys for transport security.
- Trust on first use at link time; after linking, the peer's certificate fingerprint is pinned and connections presenting another leaf certificate are rejected (`Error::FingerprintMismatch`, `Event::FingerprintChanged`).
- E2EE for payloads and encrypted local state/backups.
- Explicit linking and per-app allowlists.
- Minimal discovery payloads and opt-out discovery flags.
- App-key rotation and device-key rotation (CLI) with re-encryption/rotation guidance.
- Planned: assisted revocation and guided re-keying workflows.

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
- Assisted re-keying workflows are still pending.

## Key model (current)
- **Device keys**: per-device TLS keys used for transport security and identity fingerprints.
- **Fingerprint pinning**: the SHA-256 of the peer's leaf certificate is recorded by the `DeviceHandler` when a link is approved. On every later connection both sides bind the peer's claimed identity to the presented certificate (`is_linked_with_fingerprint`); a client that already knows the expected fingerprint (`SyncRequest::for_device`, `SyncOptions::expected_fingerprint`) additionally fails the TLS handshake itself before any application data is written. A mismatch is never re-pinned automatically: the engine emits `Event::FingerprintChanged` so the app can ask the user, and the CLI/daemon print a re-link hint.
- **App key**: shared app-level key used for payload encryption and encrypted state/backup storage.
- The app key is never optional; the engine always encrypts payloads and local state.

## Linking and key exchange (current)
- Linking occurs only between devices with the same app ID.
- The app key is exchanged during linking inside the TLS channel.
- If devices disagree on the app key, the newest linking wins and the local device adopts the remote app key.
- Linking always requires explicit consent (or explicit auto-accept configuration).
- Auto-approve linking (when enabled) will automatically link devices on private or link-local networks; do not enable on untrusted or public LANs.
- Auto-approve is time-boxed by default; long-lived auto-approve requires explicit persistence.
- Optional pairing secrets can be required for auto-approve linking; mismatched secrets reject linking.

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
Please report vulnerabilities privately through GitHub's private vulnerability
reporting: <https://github.com/dan-hart/LibreSync/security/advisories/new>.

- Do **not** open a public issue for security problems.
- Include affected versions, a minimal reproduction, and impact (what an attacker
  on the LAN, on the overlay, or with a linked device could do).
- You should receive an acknowledgement within 7 days. Fixes ship as a patch
  release with a `CHANGELOG.md` entry and a GitHub security advisory.

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

## Managed pairing boundary

Managed pairing uses mutually presented TLS certificates with SPAKE2 to prove
knowledge of an expiring QR secret or short code. Confirmations bind both TLS
fingerprints, both identities/manifests, invitation ID/version, and both PAKE
messages. This prevents a relay presenting different certificates from obtaining
a valid confirmation. QR pairing additionally verifies the pinned inviter
certificate during the TLS handshake. Only the inviter supplies the app group
key, and only after mutual password confirmation.

The selected RustCrypto `spake2` 0.4.0 implementation states that it has not had
an independent security audit and is probably not constant-time; using it does
not establish that LibreSync pairing has been independently audited. See the
[dependency security documentation](https://docs.rs/spake2/0.4.0/spake2/#security).
Short codes have limited entropy. Five challenge attempts and a five-minute
expiry limit online guessing for one invitation; users must not disclose a
code to an untrusted person or repeatedly create new codes for a suspicious
peer. Invitation `Debug` formatting omits secrets. Applications must not log
secret-bearing serialized invitations or raw protocol messages containing keys.

Malformed initial requests cannot consume an invitation. A valid challenge
counts an attempt even when its requester disconnects. A single claim is
serialized with durable approval and revocation. Pairing transport bounds
initial/pair messages to 16 KiB and uses socket timeouts. Discovery names,
schema hints, and advertised pins are never authorization evidence.

The protocol requires a durable authenticated-challenge journal on the client
and durable approval/key/trust commit callbacks on both sides. Recovery accepts
only the original journaled identity and observed TLS fingerprint with the
original pin. Applications implementing `DeviceHandler` must serialize these
callbacks, refuse silent existing-group key changes or identity/certificate
overwrite, preserve errors from unavailable secure storage, and check shutdown
and revocation in their enrollment coordinator. The low-level pairing API does
not implement application storage or an OS secure store on their behalf.

### Managed Session boundaries

Managed Session listeners deny legacy `Hello` and `LinkRequest`. Authenticated invitation enrollment serializes trust, group-key adoption, and revocation under the durable coordinator. Ordinary enrollment cannot replace a known device fingerprint or an established group's key. Explicit pinned repair retains revocation until authentication succeeds. Invitation restart recovery requires the exact journaled invitation, identity, and certificate and cannot revive local removal.

Session storage uses a stable local encryption key separate from the transport group key, private atomic temporary files, file fsync, rename, and parent-directory fsync. Prepared multi-adapter record transactions and pre-merge recovery models remain encrypted. Invalid or unavailable secure-store identity material fails closed rather than silently regenerating identity. Concurrent state-directory writers are refused. Failed/uncertain persistence fences further writes and receipts until close/reopen recovery.

Managed adapters are pure validation and merge policies over Session-owned records; they do not promise atomicity for arbitrary external app files/databases. Applications obtain a coherent inbox, commit their own transaction, then acknowledge the exact captured receipts. Store-and-forward receipt evidence is Stored, not proof of application processing.

Pause, shutdown, and removal persist generation fences and cancel/join tracked network workers. Removal preserves local records and revokes inbound/outbound identity authorization before returning. It cannot erase remote copies or a group key previously learned by an enrolled peer. Discovery labels and addresses are hints; certificate pins and authenticated full manifests establish trust. Platform permission diagnostics preserve Unknown and never infer denial from an empty discovery result.

Managed record values and encrypted batch bytes use bounded base64 JSON. Each record permits up to 64 MiB, including a serialized 32 MiB compressed application snapshot. The complete authoritative export, including metadata and receipt proof, must fit 128 MiB before encryption; the existing 256 MiB outer protocol frame cap is unchanged. Local edits, inbound merges, and recovered prepared candidates that exceed this budget fail before publication and preserve data and cursors. These are whole-state bounds; applications must partition or compact larger models before committing them.

Exact checkpoint receipts carry a 32-byte HMAC proof minted only at export. A purpose-separated HKDF key binds the application domain, full enrolled peer identity, certificate fingerprint, durable enrollment incarnation, source epoch, and sequence using length-prefixed encoding. Constant-time verification accepts legitimately delayed receipts across ordinary restart while removal, repair, and fresh enrollment rotate the incarnation. Invalid cursor hints trigger an authoritative full export; stale processing proofs cannot advance Applied and do not prevent recovery. No growing issued-checkpoint history is retained.

Pairing protocol version 2 separates authentication from durable preparation. The joiner promptly sends `PairConfirm` after verifying the server challenge. The inviter verifies it under the short initial deadline and immediately returns `PairReady`, authenticated with its separate transcript-derived ready key, without committing enrollment or releasing the app key. The joiner verifies Ready, extends only its authenticated I/O budget, durably prepares its challenge journal, and sends `PairPrepared` authenticated with the distinct prepared key. Only after verifying Prepared may the inviter commit and send the authoritative key. Missing, reflected, wrong, and replayed preparation tags cannot enroll a peer. The inviter extends its budget only after verified client proof. Version 1 is rejected explicitly; legacy Engine exchange is unchanged.

## Managed companion isolation

AlwaysOn enrolls only operator-registered exact app/schema policies using authenticated expiring invitations. Every space has independent identity, keys, storage, trust and receipts. Discovery names are untrusted text; the dashboard restricts CSP and renders names with DOM text APIs. The native macOS Keychain backend keeps binary secrets out of process arguments and treats only item-not-found as absence. Linux secure-store failures do not trigger plaintext fallback. Explicit headless file-key mode uses owner-only files and requires operator choice. Missing backup keys beside existing backups are errors, never key regeneration. Recovery exports contain sensitive unencrypted app data and require explicit UI confirmation. Legacy recovery copies import no trust or automatic approval. See [the companion contract](docs/COMPANION.md) for limits and recovery semantics.

### Tauri dependency audit qualification

The Tauri 2 lock audit reports no vulnerability entries, but retains two warnings:
`RUSTSEC-2024-0370` for unmaintained `proc-macro-error` 1.0.4 and
`RUSTSEC-2024-0429` for the unsound `glib` 0.18.5 `VariantStrIter` API.
The latter dependency belongs to the Linux GTK3 stack; its API is not called by
LibreSync or the resolved downloaded dependency sources outside glib's own API
definitions. The former is a compile-time dependency of GTK/glib macros.
This source reachability assessment does not remove the advisory or establish
that all Linux behavior is safe. GTK3 requires its matching glib version, so an
independent glib upgrade is not compatible. No audit warning is ignored; Linux
release qualification must include this known dependency limitation.
