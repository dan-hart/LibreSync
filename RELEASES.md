# Releases

## Unreleased
- See `CHANGELOG.md` for the itemised list: merge policies on the sync path,
  per-field clocks, delta sync (protocol v2), `AppendOnly` op-logs, fingerprint
  pinning with rustls 0.23, the GUI-friendly `BackgroundEngine` and wakeable
  `EventStream`, `KeyStore` backends, Linux/macOS packaging docs and service
  units, `docs/PROTOCOL.md`, and a Linux + macOS CI matrix.

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
