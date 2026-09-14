# Releases

## v0.6.1 (2026-09-14)
- Security: wire messages are capped at 256 MiB before the linking check; SQLite logical identifiers are quoted; rustls 0.23.45, crossbeam-epoch 0.9.21 and spin 0.9.9.
- First public release: crates on crates.io (`libresync`, `libresync-cli`, `libresync-ffi`), Homebrew formula in `dan-hart/homebrew-tap`, private vulnerability reporting, code of conduct and issue templates.
- Removed internal planning notes; test fixtures use documentation address ranges.

## v0.6.0 (2026-09-10)
- Merge policies now run on the sync path: inbound entries are routed to the owning adapter, and logical records carry per-field clocks so concurrent edits to different fields both survive.
- Delta sync (protocol v2): one connection per sync, only entries newer than the peer's cursor, full snapshot on first contact or after a state reset; 0.5 peers still interoperate.
- `MergePolicy::AppendOnly` for op-log fields, with event-log guidance in `docs/LOGICAL.md`.
- Fingerprint pinning after first use on both sides, `Event::FingerprintChanged`, TLS-layer pinning for known devices; rustls 0.23 without `dangerous_configuration`.
- GUI-friendly engine: `Engine::spawn()` / `BackgroundEngine`, `CancelToken`, `EventStream::raw_fd()` and wakers; FFI ABI 2 with an event queue and async sync; Swift `LibreSyncEventPump`.
- `KeyStore` trait with file, Secret Service and macOS Keychain backends.
- Linux and macOS packaging: Flatpak guidance, Tailscale degrade-once plus local-API feature, systemd and launchd units, App Sandbox entitlements, universal FFI build and Swift package tests.
- `docs/PROTOCOL.md` wire specification and a Linux + macOS CI matrix.
- Full itemised list in `CHANGELOG.md`.

## v0.5.0 (2026-06-05)
- Removed vulnerable dependency paths and moved the SQLite page delta format to a local bounded codec.
- Updated TLS, certificate, and random-number dependencies while preserving LibreSync's fingerprint-based device trust model.
- Added CI coverage for security audit, dependency audit, strict clippy, and Swift bindings.
- Fixed Swift package build metadata and AlwaysOn smoke-build warning cleanup.

## v0.4.0 (2026-03-31)
- Added a release-readiness checker, release guide, and CI smoke-build coverage so version drift gets caught before tagging.
- Fixed the AlwaysOn standalone Tauri build path and tray asset packaging for reproducible release validation.
- Tagged the `0.4.0` minor release of the `libresync` CLI.

## v0.3.0 (2026-02-09)
- Added private overlay discovery for device sync, including Tailscale/Headscale peers and static `LIBRESYNC_OVERLAY_PEERS`.
- Shipped custom logical merge hooks and SDK updates for the logical-record integration path.
- Bumped workspace crates and LibreSyncAlwaysOn app manifests to `0.3.0`.

## v0.2.0 (2026-02-09)
- Production MVP (desktop-only) with hardened stability gates.
- Linux systemd user service support for the always-on daemon.
- AlwaysOn service templates default to safe linking (no auto-approve).
- AlwaysOn status surfaces auto-approve errors and clears stale error state.
- Docs refreshed for production MVP behavior and diagnostics.
- File-backed logical adapter and record schema documentation.
- Logical record compaction policy and idempotent merge semantics updates.
- SQLite logical adapter (feature `sqlite-logical`).
- SQLite logical adapter multi-table mapping support (core + FFI array input).
- SQLite WAL/SHM integration tests and page-delta coverage.
- Backup retention planning and CLI snapshot pruning.
- CLI refresh-all, diagnostics, key rotation, and richer status output.
- CLI device-key rotation, manual address override, and multi-adapter selection.
- CLI adapter registry with JSON/SQLite support plus app/device key export/import.
- E2EE lifecycle and threat model documentation updates.
- FFI crate with Swift/Kotlin starter bindings and samples.
- SwiftData/Room-style SDK samples for multi-table SQLite logical mappings.
- LibreSyncAlwaysOn snapshot creation and retention polish.
- Auto-refresh fallback addresses for always-on reliability.

## v0.1.5 (2026-01-06)
- Engine-owned E2EE on the wire plus encrypted state/backups at rest.
- Logical record adapter example with merge policies for records.
- SQLite adapter now includes WAL/SHM and optional page-delta encoding.
- LibreSyncAlwaysOn wiring with status dashboard, manual refresh, and backup controls.
- Backup preview in CLI and explicit restore confirmation.
- SDK surface document for Swift/Kotlin bindings.
- CI coverage gate (>=75% regions).
