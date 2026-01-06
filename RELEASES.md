# Releases

## v0.1.6 (unreleased)
- File-backed logical adapter and record schema documentation.
- Logical record compaction policy and idempotent merge semantics updates.
- SQLite logical adapter (feature `sqlite-logical`).
- SQLite WAL/SHM integration tests and page-delta coverage.
- Backup retention planning and CLI snapshot pruning.
- CLI refresh-all, diagnostics, key rotation, and richer status output.
- CLI device-key rotation, manual address override, and multi-adapter selection.
- CLI adapter registry with JSON/SQLite support plus app/device key export/import.
- E2EE lifecycle and threat model documentation updates.
- FFI crate with Swift/Kotlin starter bindings and samples.
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
