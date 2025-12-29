# Security Policy

LibreSync is designed for **local-only, device-to-device synchronization** with strong privacy and security defaults. The research plan emphasizes mutual authentication, encrypted transport, explicit device trust, and minimal metadata leakage.

## Security principles
- **No cloud dependency**: connections are direct between trusted devices on LAN or an optional private overlay.
- **Mutual authentication**: devices must explicitly approve each other before syncing.
- **Encrypted transport**: all data in transit must be encrypted.
- **Least leakage**: discovery payloads should be minimal; metadata should be reduced where possible.

## Threat model (baseline)
- An attacker on the same local network attempting to eavesdrop or spoof peers.
- A malicious device attempting to join a trusted cluster.
- Replay attacks and message tampering.

## Reporting vulnerabilities
This repository does not yet have a private security contact. If you find a vulnerability:
1) Open a GitHub issue with **minimal details** and request a private channel.
2) Avoid sharing proof-of-concept exploits or sensitive data publicly.

## Security-sensitive changes
PRs affecting discovery, authentication, encryption, or key storage should include:
- A short security rationale.
- Notes on compatibility with the threat model in `RESEARCH.md`.
