# Swift managed SDK

`LibreSyncSession` owns encrypted identity, enrollment, records, recovery and reconnect scheduling. `LibreSyncEngine` remains the separate legacy low-level API.

Build all Apple slices with `scripts/build-apple-sdk.sh`, then `swift test --package-path bindings/swift`. The script honors `CARGO_TARGET_DIR`, uses the selected rustup compiler with matching cross-target standard libraries, and produces macOS arm64/x86_64, iOS arm64 and simulator arm64/x86_64 slices. CoreServices links only on macOS. The complete SDK ZIP includes the XCFramework and can be added as a local Swift package after extraction. A plain repository checkout requires building the binary first; do not treat its local binary target as a downloadable Git dependency.

```swift
let session = try await LibreSyncSession.open(
    .notes(stateDirectory: directory, displayName: "My device"),
    keys: LibreSyncKeychainStore(service: "io.libresync.Notes"))
let model = await LibreSyncSessionModel(session: session)
await model.start()
```

The notes convenience uses exactly the companion's public `notes_manifest`. A custom application chooses `policy: .records` and its own stable `LibreSyncManifest` app ID, namespace and `logical-records-v1` descriptors. This explicitly selects the reviewed pure logical LWW/tombstone policy. `momentum` selects the exact verified immutable Momentum policy. Other custom schemas are refused with `InvalidAdapter`; register their pure `ManagedAdapter` in Rust instead. Session owns storage; policies must not write an external database.

Use `LibreSyncConnectView`, `LibreSyncDevicesView`, and `LibreSyncStatusView` with one shared model. They include offline QR display/optional camera scanning, actual nearby friendly devices and six-digit codes, explicit Combine/Cancel, pause/resume, repair and data-preserving removal. `Samples/QuickstartApp` contains a SwiftUI application source and Info.plist. Copy it into an app target and add the extracted SDK package. The UI does not establish trust from Bonjour hints; core authentication checks pinned certificates, pairing version and the complete manifest.

For ordinary iOS discovery, declare `NSLocalNetworkUsageDescription` and `NSBonjourServices = ["_libresync._tcp"]`. Optional QR scanning also needs `NSCameraUsageDescription`. The SDK defaults to system `NetService` Bonjour on the actual Rust listener, with raw core multicast disabled. It needs no restricted multicast entitlement for that fixed declared service. Default listeners use IPv4; resolved AAAA hints are retained only for an explicitly configured IPv6 listener. Scoped IPv6 values use numeric scope IDs. IPv6 reachability is not claimed from the IPv4 native test. Raw BSD multicast is an advanced core opt-in requiring the restricted entitlement on physical iOS.

A permission timeout or an empty nearby list means `Unknown`; only actual platform policy evidence means `Denied`. Simulator tests cannot verify physical iOS Local Network privacy. Pause when backgrounded and resume/wake on foreground; iOS suspension limits background work. Stop the model and await `session.close()` when the host is finished.

```swift
let inbox = try await session.inbox()
// Persist records AND captured receipts, including every opaque 32-byte proof,
// in one durable app transaction. This sample fsyncs file and parent directory.
try await Task.detached { try LibreSyncInboxJournal.save(inbox, to: appFile) }.value
try await session.acknowledge(inbox) // only after successful durable app save
```

All blocking native work runs off the main thread. `events()` is a cancellable AsyncSequence; stopping observation does not cancel shared networking. Cancelling a connect/repair asks core to cancel current network operations. Explicit `cancel()` is session-wide for currently registered operations, and the scheduler may retry later; use pause for sustained suspension and close to fence new work. Concurrent close callers await the same cleanup. Events are snapshot-change notifications, with a 256-event newest buffer rather than a durable transaction log. Each observation gets an independent bounded native queue; stopping one subscriber preserves the others.

Keychain `not found` is distinct from locked, unavailable or corrupt storage. There is no plaintext fallback or silent identity regeneration. Errors use `LibreSyncSessionErrorCode`, with separate oversized/invalid record, incompatible version, revoked peer, group conflict and storage errors. Record bytes and proofs use exact base64 Codable `Data`; UInt64 counters retain their full precision.

`scripts/package-native-sdk.sh 0.7.0` builds and packages all five Apple architectures and both Android ABIs. The complete `LibreSyncSwift-0.7.0.zip` has a local XCFramework manifest, sources and tests and can be used as an extracted path dependency. The generated `Package.release.swift` is a reviewable Git-consumer manifest with the checksum of the exact XCFramework ZIP; install it at the repository root only after those release assets are available. Until then, cloning the Git source alone requires building the XCFramework first.

After durably saving the complete coherent inbox, acknowledgement sends only its captured revision and exact source receipt map; record bytes stay in the saved app transaction and are not retransmitted in the acknowledgement request. This keeps a near-limit valid record aggregate acknowledgeable even with multiple proof-bearing source receipts. The C command accepts an optional legacy `records` field but does not use it. Cancellation before a queued Swift call starts prevents native work; cancellation after start interrupts current IO on a best-effort basis and is not transaction rollback.
