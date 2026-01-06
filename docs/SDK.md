# SDK surface (Swift/Kotlin)

## Goals
- Provide a stable, minimal API for apps to embed LibreSync.
- Keep platform-specific behavior in thin Swift/Kotlin wrappers.
- Guarantee E2EE by default with no opt-out.

## Core objects
- `Identity`: `device_id`, `app_id`, `user_id`.
- `EngineConfig`: identity + optional listen address.
- `Engine`: pairing, discovery, refresh, and event stream.
- `SyncRecord`: logical record (`schema`, `entity`, `id`, `fields`, `tombstone`, `clock`).
- `State`: encrypted local sync state (managed by the engine).

## Engine lifecycle
- `engine_new(config, state_path)` -> Engine handle.
- `engine_register_adapter(adapter)` -> register file/logical adapters.
- `engine_start_listening(listen_addr)` / `engine_stop_listening()`.
- `engine_discover(timeout)` -> list visible devices on LAN.
- `engine_request_pair(address)` -> pair with a device (consent required).
- `engine_sync_now(address, adapter_id)` -> refresh data for one adapter.
- `engine_auto_refresh(config)` -> background refresh loop.

## Adapter contracts
- File adapter: JSON file or SQLite file (plus WAL/SHM).
- Logical adapter: record-level sync with merge policies.
- Adapter registration is per engine instance; adapters are identified by `adapter_id`.

## Events
- Event stream is exposed as a pollable queue.
- Events include:
  - `PairingRequested`, `PairingDecisionRequired`
  - `SyncStarted`, `SyncFinished`
  - `DeviceSeen`, `DeviceOffline`, `Error`
- No cross-thread callbacks from Rust; wrappers poll the queue and dispatch on the platform thread.

## Backups
- `backup_create(adapter_id, note)` -> encrypted snapshot.
- `backup_list(adapter_id)` -> metadata list.
- `backup_preview(adapter_id, snapshot_id)` -> diff summary.
- `backup_restore(adapter_id, snapshot_id, confirm_id)` -> restore with explicit confirmation.
- Backups are opt-in per app; restores require an additional allow-restore flag.

## Threading and safety
- FFI calls must be thread-safe and non-blocking where possible.
- Long-running work (auto-refresh, listener, discovery) runs on Rust-managed threads.
- Wrappers should surface cancellation hooks and map errors into platform-native types.

## Packaging
- Swift: package as an XCFramework + Swift Package.
- Kotlin: package as an AAR with JNI bindings.
- Prefer UniFFI for generation; fall back to a C ABI if needed for control.
