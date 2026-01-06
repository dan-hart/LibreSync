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
See `sample/Main.kt` for a minimal usage example.

## Notes
- The Kotlin wrapper uses JNI-style `external` functions.
- Convert JSON strings into your own data classes as needed.
- Logical record adapters (`registerLogicalFileAdapter`) are the recommended integration path for structured data.
- Android requires Local Network permissions for discovery:
  - `android.permission.INTERNET`
  - `android.permission.CHANGE_WIFI_MULTICAST_STATE` (mDNS)
  - acquire a `MulticastLock` while discovering on Wi-Fi.
- Store app keys and device keys in Android Keystore (see `sample/AndroidKeyStoreStorage.kt`).
- `LibreSyncKeyStorage` provides a simple interface you can back with secure storage.
