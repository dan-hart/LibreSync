# Progress log

## Summary
LibreSync has a working device-to-device MVP for two CLI instances on a LAN. Devices can discover each other, pair with consent, and refresh a single JSON file with deterministic conflict resolution.

## Completed
- Rust workspace with core library and CLI.
- Device identity model (device/app/user IDs) with per-app trust.
- LAN discovery via mDNS.
- Pairing flow (trust only) and refresh flow (data exchange).
- Device listener for inbound connections.
- CLI commands: init, select, discover, pair, listen, refresh, watch, status.
- Status output with last-seen timestamps.
- Listener runs in the background by default with PID/log output.
- Discovery filters out the local device from selection lists.
- Verbose error output available via `--verbose`.
- Architecture and CLI docs in `docs/`.
- Test suite covering core and CLI flows.
- SDK-first `Engine` API with adapters and device discovery/pairing.
- CLI dogfoods the `Engine` for listen, discover, pair, refresh, and watch.
- Frankly Tauri demo app with SQLite-backed todos and LAN device linking.
- Engine auto-refresh loop for polling-based change sync.
- Vision and DX/UX goals captured in README and research docs.
- Watched file adapter for near-real-time local change detection.
- Logical record types + adapter scaffolding in core.
- Mandatory app-level encryption for sync payloads (engine-owned E2EE on the wire).
- Encrypted state files and backups at rest using the app-level key.
- SQLite sync now includes WAL/SHM sidecar data alongside snapshots.
- SQLite adapter supports optional page-delta encoding to reduce payload size.
- In-memory logical adapter added for record-level merges and policy testing.
- LibreSyncAlwaysOn planning docs added (always-on desktop device).
- LibreSyncAlwaysOn now includes status dashboard, manual refresh, and snapshot controls.
- SDK surface document added for Swift/Kotlin bindings.
- Expanded unit test coverage (75%+) and bumped version to 0.1.5.

## Current MVP behavior
- Two devices on the same LAN can run `libresync`.
- Devices auto-discover via mDNS and can be selected interactively.
- Pairing requires consent on both devices and adds to the allowlist.
- Sync exchanges a single JSON file and applies last-writer-wins via Lamport clocks.

## Known gaps
- Device keys are self-signed and stored locally; no rotation/revocation or shared trust roots yet.
- Pairing uses fingerprint allowlisting (TOFU) without out-of-band verification prompts.
- Single-file refresh only (one JSON file mapped to the `file` key).
- No logical record adapter implementations shipped yet (scaffolding only).
- SQLite WAL/SHM is synced as files; page-delta merge logic is still needed.
- Background auto-refresh is polling-based and relies on mDNS discovery.
- No always-on desktop device implementation yet (LibreSyncAlwaysOn app still pending).

## Next steps
- Ship logical record sync (mergeable/CRDT-ish) as the primary adapter path.
- Add adapter capabilities for snapshot + delta + watch.
- Implement SQLite page-delta merge logic on top of WAL.
- Extend E2EE with key rotation and re-keying flows.
- Plan FFI boundary + Swift/Kotlin wrapper approach for cross-platform adapters.
- Build LibreSyncAlwaysOn (LAN-only, Rust + Tauri) as an always-on device.
- Add key rotation/export, fingerprint display UX, and re-pairing flows.

## Tests
- `cargo test` covers core and CLI behavior.
