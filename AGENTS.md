# Repository Guidelines

## Project Structure
- `crates/libresync` — sync engine core (protocol, crypto, adapters, discovery).
- `crates/libresync-cli` — `libresync` command-line tool; dogfoods the `Engine` API.
- `crates/libresync-ffi` — C ABI wrapper consumed by `bindings/swift` and `bindings/kotlin`.
- `crates/libresync-alwayson-daemon` — headless always-on device; `alwaysOn/` holds the Tauri tray app.
- `docs/` — architecture, protocol, API stability, packaging, and release guides.
- `contrib/` — systemd, launchd, and Flatpak templates. `scripts/` — build and audit helpers.
- Root policy docs: `README.md`, `SECURITY.md`, `PRIVACY.md`, `CONTRIBUTING.md`, `CHANGELOG.md`, `RELEASES.md`, `LICENSE`.

## Build, Test, and Verify
- `cargo build --workspace`
- `cargo test --workspace` and `cargo test -p libresync --all-features`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` (CI gate)
- `./scripts/utilities/check-release-readiness.sh` and `./scripts/utilities/security-audit.sh` before tagging.
- See `TESTING.md` for the two-device and coverage flows and `docs/RELEASING.md` for the release checklist.

## Coding Style
- Rust 2021, `rustfmt` defaults, no clippy warnings.
- Errors flow through `libresync::Error`; avoid `unwrap`/`expect` outside tests.
- Markdown: short paragraphs, bullet lists, sentence-case headings.

## Commits and Pull Requests
- Short, imperative commit summaries (e.g. "Add delta sync cursor migration").
- Every user-visible change gets a `CHANGELOG.md` entry under `[Unreleased]`.
- PRs touching discovery, linking, encryption, key storage, or the wire format must include a security rationale and update `SECURITY.md` / `docs/PROTOCOL.md` as needed.

## Security and Configuration
- LibreSync is local-only by design; do not add cloud or relay dependencies without explicit discussion.
- Never commit keys, config directories, or state files; `.gitignore` and `scripts/utilities/security-audit.sh` enforce this.
