# Kotlin SDK

This folder provides a minimal Kotlin wrapper around the `libresync-ffi` C ABI.

## Build the FFI library
From the repo root:
```
cargo build -p libresync-ffi
```

## Usage
- Ensure the `libresync_ffi` shared library is available on the runtime path.
- Use the wrapper in `src/main/kotlin/LibreSync.kt`.

## Example
- `sample/Main.kt`: minimal logical-file setup.
- `sample/RoomSample.kt`: SQLite logical sync with multi-table mappings for a Room-style schema.

## Notes
- The Kotlin wrapper uses JNI-style `external` functions.
- Convert JSON strings into your own data classes as needed.
- Logical record adapters (`registerLogicalFileAdapter`) are the recommended integration path for structured data.
- `LibreSyncKeyManager` can generate and store keys via `LibreSyncKeyStorage` and produce a JSON config.
- `registerSqliteLogicalAdapter` supports mapping existing SQLite tables (via `LibreSyncSqliteLogicalMapping`).
- `registerSqliteLogicalAdapter(..., mappings)` accepts multiple table mappings in one adapter.
- Android requires Local Network permissions for discovery:
  - `android.permission.INTERNET`
  - `android.permission.CHANGE_WIFI_MULTICAST_STATE` (mDNS)
  - acquire a `MulticastLock` while discovering on Wi-Fi.
- Store app keys and device keys in Android Keystore (see `sample/AndroidKeyStoreStorage.kt`).
- `LibreSyncKeyStorage` provides a simple interface you can back with secure storage.
