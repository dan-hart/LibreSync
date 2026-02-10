# Progress log

## Summary
LibreSync has a working device-to-device MVP for two CLI instances on a LAN. Devices can discover each other, link with consent, and refresh JSON or SQLite adapters with deterministic conflict resolution.

## Status snapshot
- Core sync engine: strong (linking, discovery, refresh, auto-refresh, event stream).
- Security/E2EE: strong (app key encryption, key export/import/rotation).
- File adapters (JSON/SQLite/WAL/SHM): strong (page-delta optional, solid tests).
- Logical records (mergeable state): moderate (schema and merges exist; still early for real apps).
- SDK bindings (Swift/Kotlin): early-to-moderate (wrappers + key storage helpers + SQLite logical mapping samples; still not production-ready).
- Always-on desktop device: MVP (UI, snapshots, tray, trust panel; Linux daemon/service runbook shipped).
- DX/UX docs + examples: good (docs are coherent; SwiftData/Room schema samples added).

## Completed
- Rust workspace with core library and CLI.
- Device identity model (device/app/user IDs) with per-app trust.
- LAN discovery via mDNS.
- Linking flow (trust only) and refresh flow (data exchange).
- Device listener for inbound connections.
- CLI commands: init, select, discover, link, listen, refresh, watch, status.
- Status output with last-seen timestamps.
- Listener runs in the background by default with PID/log output.
- Discovery filters out the local device from selection lists.
- Verbose error output available via `--verbose`.
- Architecture and CLI docs in `docs/`.
- Test suite covering core and CLI flows.
- SDK-first `Engine` API with adapters and device discovery/linking.
- CLI dogfoods the `Engine` for listen, discover, link, refresh, and watch.
- Engine auto-refresh loop for polling-based change sync.
- Vision and DX/UX goals captured in README and research docs.
- Watched file adapter for near-real-time local change detection.
- Logical record types + adapter scaffolding in core.
- Mandatory app-level encryption for sync payloads (engine-owned E2EE on the wire).
- Encrypted state files and backups at rest using the app-level key.
- SQLite sync now includes WAL/SHM sidecar data alongside snapshots.
- SQLite adapter supports optional page-delta encoding to reduce payload size.
- In-memory logical adapter added for record-level merges and policy testing.
- File-backed logical adapter added for JSON record persistence.
- Logical record schema and merge policy documentation added.
- Logical record compaction policy added with tombstone pruning.
- Counter/list merge semantics hardened for idempotent state-based sync.
- LibreSyncAlwaysOn planning docs added (always-on desktop device).
- LibreSyncAlwaysOn now includes status dashboard, manual refresh, and snapshot controls.
- LibreSyncAlwaysOn can now create snapshots from the UI and stores last-seen device addresses.
- SDK surface document added for Swift/Kotlin bindings.
- Expanded unit tests and bumped version to 0.1.5.
- SQLite WAL/SHM integration tests added (real SQLite files).
- Backup retention planning and CLI pruning support added.
- CLI refresh-all and richer status output added.
- CLI diagnostics and app-key rotation added.
- CLI device-key rotation and manual address override added.
- CLI supports multiple JSON adapters with per-adapter selection.
- CLI adapter registry now supports JSON and SQLite adapters.
- CLI adapter registry now supports logical record files.
- App/device key export/import flows added to the CLI.
- Auto-refresh now supports fallback addresses when discovery is unavailable.
- E2EE lifecycle and threat model docs expanded.
- API stability, quickstart, and debugging docs added.
- FFI crate added with Swift/Kotlin starter bindings and samples.
- SQLite logical mapping added with sidecar metadata for existing tables.
- SQLite logical mapping now supports JSON-value encoding and per-field merge policies.
- SQLite logical mapping now supports multi-table mappings in a single adapter.
- Logical adapters now support app-defined `MergePolicy::Custom` merge hooks.
- Swift/Kotlin binding docs now cover Local Network permissions and secure key storage.
- LibreSyncAlwaysOn now supports system tray show/hide and trust UI (linking + fingerprints).
- Swift/Kotlin samples now default to logical record adapters.
- FFI tests added to meet the coverage gate.
- FFI now exposes key generation helpers for SDK key storage.
- Swift/Kotlin SDKs include key manager + config builders and SQLite logical mapping registration.
- Headless LibreSyncAlwaysOn daemon added with service templates for OS-level background runs.
- Auto-approve linking mode added (opt-in, private/link-local LAN only).
- Coverage gate met with additional CLI/FFI/daemon tests (backup, keys, auto-approve, daemon config/event handling).
- 0.2.0 production MVP hardening and Linux systemd user service runbook.
- Swift/Kotlin SDK samples now include SwiftData/Room-style SQLite schema mappings.

## Current MVP behavior
- Two devices on the same LAN can run `libresync`.
- Devices auto-discover via mDNS and can be selected interactively.
- Linking requires consent on both devices and adds to the allowlist.
- Sync exchanges selected JSON or SQLite adapters and applies last-writer-wins via Lamport clocks.

## Known gaps
- Device keys are self-signed and stored locally; rotation is CLI-only and needs SDK UX for verification.
- Linking uses fingerprint allowlisting (TOFU) without out-of-band verification prompts.
- Logical record sync is now the preferred path but still early for production apps.
- SQLite logical mapping is available (including multi-table mappings) but still evolving for complex relational schemas.
- Background auto-refresh is polling-based; discovery is mDNS-first with manual fallback addresses.
- LibreSyncAlwaysOn has Linux daemon/service support; macOS/Windows still rely on tray behavior.
- Coverage gate is met but additional tests would improve confidence across the SDK/FFI surface.

## Next steps
- Improve SDK UX for key verification and guided trust/re-key flows.

## Tests
- `cargo test` covers core and CLI behavior.
