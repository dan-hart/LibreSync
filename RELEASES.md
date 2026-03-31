# Releases

## v0.4.0 (2026-03-31)
- Added a release-readiness checker, release guide, and CI smoke-build coverage so version drift gets caught before tagging.
- Fixed the AlwaysOn standalone Tauri build path and tray asset packaging for reproducible release validation.
- Published the first Homebrew tap formula for the `libresync` CLI alongside the `0.4.0` minor release.

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
