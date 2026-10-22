# Native Nearby Sync implementation plan

**Goal:** Reusable native Apple Nearby Sync and a runnable macOS notes demo backed by real Rust sessions.

**Approved design:** Shared Rust behavior, platform SDK integration, optional native UI, app-owned data application. Names are stable per app installation and never grant trust. Pairing stays authenticated; application receipt acknowledgement stays durable. First slice targets macOS with reusable iOS-compatible SwiftUI components; Android/Linux follow later.

## Task 1: Shared friendly identity

- [x] Test generated names, restart stability, install isolation and explicit names.
- [x] Rust resolves empty display_name once using OS randomness and the encrypted journal after acquiring the lease; preserve explicit names and identity. No dependencies or wire changes.
- [x] Test Session and FFI empty-name acceptance; document security boundaries. Keep incoming DeviceMetadata validation strict. Resolve local aliases before constructing both config and envelope metadata. Verify invitations/discovery agree after restart, including existing journals and explicit caller names.

## Task 2: Native SDK experience

- [x] Test discovery compatibility independently from pairing availability, expiry, code validation and truthful status.
- [x] Extend LibreSyncSessionModel with busy/error/pairing state and awaitable lifecycle; retain old view APIs.
- [x] Add reusable SwiftUI Nearby Sync: local name, connected devices, nearby devices, authenticated expiring code/QR invitation, merge consent, permission/recovery help. No auto-open pairing.
- [x] Fence duplicate actions, expiry, dismissal and cancellation; close pairing when dismissed. Hide technical diagnostics behind Details. Support accessibility, keyboard entry, light/dark, macOS 12/iOS 15.

## Task 3: macOS demo

- [x] SwiftPM executable at demos/NearbySyncMac: notes plus the SDK Nearby Sync screen.
- [x] Two optional profiles with distinct private Application Support and Keychain namespaces, same manifest. No plaintext key fallback.
- [x] Save coherent inbox records/receipts with LibreSyncInboxJournal before acknowledgement; surface persistence failures.
- [x] Build script bundles local macOS Rust FFI in signed .app with Bonjour/Local Network declarations; launch-one/two instructions without manual Xcode setup.

## Validation

- [x] Baseline/final Swift tests using local FFI, Rust workspace/all-feature tests, formatting and clippy.
- [x] Build/launch .app; exercise real discovery, code pairing, notes delivery, durable acknowledgement. Report privacy/UI validation limitations.
- [x] Independent plan, spec-compliance and code-quality review, fix substantive findings.
- [x] CHANGELOG Unreleased and SDK/demo docs. No publication/tagging.

## Validation outcome

Rust workspace build/tests, all-feature engine tests, formatting and strict Clippy passed. The final Swift SDK suite passed 31 tests. The six demo tests include actual Bonjour, authenticated code pairing, explicit populated merge consent, production app note persistence/Applied receipts, authenticated rename and reopen. The app built, passed ad-hoc signature verification, and its native screen was inspected. GUI click-through pairing could not finish after the computer-use native pipe closed; physical iOS/network privacy remains unverified. An existing three-second Bonjour expiry assertion failed once, then passed unchanged in isolation and the full suite; no timing fix is claimed.
