# LibreSync
- Library enabling device to device data syncronization without connecting to the cloud.

## What is this?
- LibreSync is an open-source AGPLv3 Rust framework that enables real-time and eventual-consistency synchronization of structured application data directly between devices on the same local network (and optional private overlay) without touching the internet.

## Who is this for?
- Developers who want to add data synchronization to their desktop or mobile app, but don't want to mess with a server.
- Privacy-concious users looking for low-level structured data sychronization.
- Researchers: this library can be used to keep several machine's data in sync.

## Why does this exist?
- The cloud _sucks_, leave it behind and unleash local, direct information sharing.
- We are tired of account or service-based sync, I want easy, free, and local sync.
- All devices should be able to share data if consent is given.

## Where can I use this?
- In Rust. This repo is _only_ for the shared sync engine logic.
- Soon, platform-specific libraries will be added to enable application integration.
- As of January 2026, the project is in alpha.

---

## Values
- Privacy: a human right
- Security: end-to-end encryption
- Freedom: use this library for free, forever

## License
AGPLv3 - Why? Because it's what we decided upon.

## Security & Privacy Checks
- Install git-secrets and hooks:
  - `brew install git-secrets`
  - `git secrets --install`
  - `git secrets --register-aws`
- Or run the one-shot setup: `./scripts/automation/setup-repo-security.sh .`
- Install the ASP pre-commit hook: `./scripts/automation/install-asp-hooks.sh .`
- Run a full audit before pushing: `./scripts/utilities/security-audit.sh`
- See `SECURITY.md`, `PRIVACY.md`, and `CONTRIBUTING.md` for full guidance.

## Additional documents
- [Research](RESEARCH.md)
- [Security](SECURITY.md)
- [Privacy](PRIVACY.md)
- [Contributing](CONTRIBUTING.md)
- [License](LICENSE)
