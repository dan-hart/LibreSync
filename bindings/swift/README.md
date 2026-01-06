# Swift SDK

This package wraps the `libresync-ffi` C ABI with a small Swift API.

## Build the FFI library
From the repo root:
```
cargo build -p libresync-ffi
```

The compiled library will be in `target/debug/` (or `target/release/`).

## Usage
- Add `bindings/swift` as a Swift Package dependency.
- Link the `libresync-ffi` library at build time.

## Example
See `Samples/QuickstartApp/main.swift` for a minimal usage example.

## Notes
- The Swift wrapper expects a JSON config that includes device keys and app key.
- Use `LibreSyncEngine.lastError()` for details if a call fails.
- Logical record adapters (`registerLogicalFileAdapter`) are the recommended integration path for structured data.
- iOS/macOS require Local Network permission for discovery and inbound connections:
  - `NSLocalNetworkUsageDescription`
  - `NSBonjourServices` with `_libresync._tcp`
- Use `LibreSyncLocalNetworkPermission` to trigger the system prompt.
- Store app keys and device keys in the Keychain (see `LibreSyncKeychain`).
