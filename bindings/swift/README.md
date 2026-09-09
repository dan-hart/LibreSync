# Swift SDK

This package wraps the `libresync-ffi` C ABI with a small Swift API.

## Build the FFI library
From the repo root, build the universal (arm64 + x86_64) static library and
the XCFramework the package links against:
```
rustup target add aarch64-apple-darwin x86_64-apple-darwin
scripts/build-macos-universal.sh
cd bindings/swift && swift build && swift test
```

## Usage
- Add `bindings/swift` as a Swift Package dependency (the XCFramework must be
  built or downloaded first; see `docs/PACKAGING-MACOS.md`).
- Entitlements: `com.apple.security.network.client` and `.server`, plus the
  Local Network `Info.plist` keys.
- Events: `engine.makeEventPump { event in ... }` delivers events on the main
  queue through a `DispatchSource` on the engine's event descriptor; use
  `syncAsync` for non-blocking syncs and `cancel(ticket:)` to abort.

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
