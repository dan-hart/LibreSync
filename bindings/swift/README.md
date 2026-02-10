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
- `Samples/QuickstartApp/main.swift`: minimal logical-file setup.
- `Samples/SwiftDataNotes/main.swift`: SQLite logical sync with multi-table mappings for a SwiftData-style schema.

## Notes
- The Swift wrapper expects a JSON config that includes device keys and app key.
- Use `LibreSyncEngine.lastError()` for details if a call fails.
- Logical record adapters (`registerLogicalFileAdapter`) are the recommended integration path for structured data.
- `LibreSyncKeyManager` can generate and store keys in the Keychain and return a `LibreSyncConfig` you can encode to JSON.
- `registerSqliteLogicalAdapter` supports mapping existing SQLite tables (via `LibreSyncSqliteLogicalMapping`).
- `registerSqliteLogicalAdapter(..., mappings: [...])` accepts multiple table mappings in one adapter.
- iOS/macOS require Local Network permission for discovery and inbound connections:
  - `NSLocalNetworkUsageDescription`
  - `NSBonjourServices` with `_libresync._tcp`
- Use `LibreSyncLocalNetworkPermission` to trigger the system prompt.
- Store app keys and device keys in the Keychain (see `LibreSyncKeychain`).
