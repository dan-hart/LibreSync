# SDK surface (Swift/Kotlin)

## Goals
- Provide a stable, minimal API for apps to embed LibreSync.
- Keep platform-specific behavior in thin Swift/Kotlin wrappers.
- Guarantee E2EE by default with no opt-out.

## Status
- Swift/Kotlin bindings are early and not yet production-ready.
- Local Network permission helpers and secure key storage helpers are available, but full integration UX is still pending.

## Core objects
- `Identity`: `device_id`, `app_id`, `user_id`.
- `EngineConfig`: identity + optional listen address.
- `Engine`: linking, discovery, refresh, and event stream.
- `SyncRecord`: logical record (`schema`, `entity`, `id`, `fields`, `tombstone`, `clock`).
- `State`: encrypted local sync state (managed by the engine).

## Engine lifecycle
- `engine_new(config, state_path)` -> Engine handle.
- `engine_register_adapter(adapter)` -> register file/logical adapters.
- `engine_start_listening(listen_addr)` / `engine_stop_listening()`.
- `engine_discover(timeout)` -> list visible devices on LAN.
- `engine_request_link(address)` -> link with a device (consent required).
- `engine_sync_now(address, adapter_id)` -> refresh data for one adapter.
- `engine_auto_refresh(config)` -> background refresh loop.
- `engine_auto_refresh_with_config(config)` -> supports fallback addresses and discovery tuning.

## Adapter contracts
- File adapter: JSON file or SQLite file (plus WAL/SHM, optional page-delta).
- Logical adapter: record-level sync with merge policies.
- Adapter registration is per engine instance; adapters are identified by `adapter_id`.
- SQLite logical mappings can target existing tables with a sidecar metadata table for clocks/tombstones.
- One adapter can register multiple SQLite logical mappings to sync multi-table schemas in one refresh cycle.
- Swift/Kotlin wrappers can map SwiftData/Room tables through SQLite logical mappings to keep record-level semantics.

## Events
- Event stream is exposed as a pollable queue.
- Events include:
  - `LinkingRequested`, `LinkingDecisionRequired`
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

## Key handling
- The engine manages app-level keys; E2EE is enforced and cannot be disabled.
- Linking exchanges the app key inside the encrypted transport.

## Platform constraints to plan for
- iOS and macOS require Local Network permission for discovery and inbound connections.
- Some Wi-Fi networks enable AP isolation, blocking device-to-device traffic.
- Mobile platforms restrict background execution; wrappers should re-sync quickly on resume.

## Secure key storage
- SDK wrappers should store app keys and device keys in Keychain (Apple) or Keystore (Android).
- This repo includes Swift Keychain helpers and an Android Keystore storage sample to build on.
- The C ABI now exposes key generation helpers to seed secure storage and build config JSON on-device.
- File-based key storage is acceptable only for development and the CLI.

## FFI boundary plan
The FFI layer should be small, versioned, and handle-based so wrappers stay thin and stable.

**Surface area (minimum viable)**
- `libresync_abi_version()` -> integer ABI version.
- `libresync_generate_app_key()` -> base64 app key.
- `libresync_generate_device_keys(device_id, app_id, user_id)` -> JSON with base64 device cert/key + fingerprint.
- `engine_create(config_json, state_path)` -> handle.
- `engine_free(handle)`.
- `engine_register_file_adapter(handle, adapter_id, path)`.
- `engine_register_logical_adapter(handle, adapter_id, namespace)`.
- `engine_start_listening(handle, listen_addr)` / `engine_stop_listening(handle)`.
- `engine_register_sqlite_logical_adapter(handle, adapter_id, namespace, path, mapping_json)` -> map existing SQLite tables (`mapping_json` accepts one mapping object or an array).
- `engine_discover(handle, timeout_ms)` -> list of devices.
- `engine_request_link(handle, address)` / `engine_accept_link(handle, device_id, decision)`.
- `engine_sync_now(handle, address, adapter_id)`.
- `engine_event_next(handle)` -> next event (pollable).
- `backup_create(handle, adapter_id, note)` / `backup_list(handle, adapter_id)` / `backup_preview(handle, adapter_id, snapshot_id)` / `backup_restore(handle, adapter_id, snapshot_id, confirm_id)`.

**Types and ownership**
- Use opaque handles for engine and adapters.
- Use UTF-8 strings with explicit ownership (caller frees returned strings).
- Use stable enums for error codes and event kinds.

**Threading**
- No cross-thread callbacks from Rust.
- Wrappers poll the event queue and dispatch on the platform thread.

**Versioning**
- ABI version must be pinned and incremented on breaking changes.
- SDKs should validate ABI version at startup and return a clear error if mismatched.

## FFI config (current)
The C ABI expects a JSON config with device keys and app key encoded in base64. Optional fields can be omitted:

```
{
  "device_id": "device-a",
  "app_id": "com.example.app",
  "user_id": "user-a",
  "listen_addr": "0.0.0.0:52345",
  "app_key": "<base64>",
  "device_cert_der": "<base64>",
  "device_key_der": "<base64>",
  "allowlist": [
    { "device_id": "device-b", "fingerprint": "..." }
  ],
  "auto_accept": false,
  "pairing_secret": "optional-shared-secret"
}
```

Notes:
- `auto_accept` enables automatic link approval; keep it off on untrusted networks.
- `pairing_secret` (optional) requires link requests to include the same shared secret.

## Packaging
- Swift: package as an XCFramework + Swift Package.
- Kotlin: package as an AAR with JNI bindings.
- Prefer UniFFI for generation; fall back to a C ABI if needed for control.

## Bindings in this repo
- `crates/libresync-ffi` provides a minimal C ABI.
- `bindings/swift` wraps the C ABI in a Swift Package.
- `bindings/kotlin` provides a JNI-style wrapper and sample usage.
- Sample apps include `bindings/swift/Samples/SwiftDataNotes/main.swift` and `bindings/kotlin/sample/RoomSample.kt`.
