# Swift managed SDK

`LibreSyncSession` owns encrypted identity, enrollment, records, recovery and reconnect scheduling. `LibreSyncEngine` remains the separate legacy low-level API.

Install the Git Swift package at exact version `0.8.0` from `https://github.com/dan-hart/LibreSync.git`, or extract `LibreSyncSwift-0.8.0.zip` and add its local package. Add the `LibreSync` product to your app target. The release package includes macOS arm64/x86_64, iOS arm64 and Simulator arm64/x86_64 slices. CoreServices links only on macOS. For a source checkout, run `scripts/build-apple-sdk.sh`, then use `LIBRESYNC_USE_LOCAL_XCFRAMEWORK=1 swift test` at the repository root. The build script honors `CARGO_TARGET_DIR` and uses the selected rustup compiler with matching cross-target standard libraries.

## Integrate Nearby Sync in an app

An open-source app can embed the v0.8.0 screen without adopting the demo's note model. Choose a stable app ID, namespace, schema and supported policy first. `.notes` below is a runnable notes-contract example; As Needed and Momentum must choose their own reviewed integration contract. Use `.records` with `LibreSyncManifest` for supported transactional logical records, or the exact `.momentum` contract when appropriate. Sharing the screen alone does not make different app contracts compatible.

Create one session and one model per app space, retain them in the app's host, and present the same model from Settings:

```swift
let session = try await LibreSyncSession.open(
    .notes(stateDirectory: directory),
    keys: LibreSyncKeychainStore(service: "io.libresync.Notes"))
let model = await LibreSyncSessionModel(session: session)
await model.startAndWait()
// In the host's SwiftUI content:
LibreSyncNearbySyncView(model: model)
```

Use a private, persistent Application Support directory and a stable Keychain service. Declare the Local Network/Bonjour keys described below, plus the camera key if enabling scanning. Omit `displayName` to use Rust's persistent generated name. On backgrounding, await `model.stopAndWait()`; foreground with `startAndWait()`. Before replacing or disposing of the host, stop the model and then await `session.close()`.

The host must process the current `session.inbox()` after startup and when session events signal changes. Events are bounded hints; always fetch a fresh coherent inbox and serialize app writes. In one durable app transaction, apply records/tombstones and retain the inbox revision and exact receipts/proofs; acknowledge that captured inbox only after the transaction succeeds. Retry from durable state after failure or restart. The persistence example below shows this boundary for the notes sample; substitute the app's own database transaction. Keep local edit clocks and publishing coordinated with the same host. See the [shared demo host](../../demos/NearbySyncMac/Sources/NearbySyncMac/DemoModel.swift) and [storage implementation](../../demos/NearbySyncMac/Sources/NearbySyncMac/DemoStorage.swift) for a complete reference.

The SDK is AGPL-3.0-only; review [the license](../../LICENSE) when distributing an integration. This SDK release does not change As Needed or Momentum, apply the pinned Momentum patch, or automatically migrate an app's existing data.

## Native Nearby Sync

### Customize the accent

The SDK inherits the host SwiftUI tint; it does not impose the demo app’s orange accent. Set a tint on the screen or an ancestor to style its buttons and accent icons, including pairing sheets:

```swift
LibreSyncNearbySyncView(model: model)
    .tint(.purple) // Or Color("BrandAccent") from the host app.
```

The standalone Connect, Devices and Status components inherit the same host styling. Success, error and destructive controls retain their semantic system colors; QR codes retain black/white contrast for scanning.


Use `LibreSyncNearbySyncView(model: model)` for a guided native flow: a stable friendly device name, Your devices, explicit Add Device, expiring six-digit codes or QR invitations, and connection help. It works with one shared `LibreSyncSessionModel`. The screen owns its scrolling: embed it directly in the host navigation content, without another ScrollView or List. Give the host page its Nearby Sync navigation title. `LibreSyncDevicesView` contains non-scrolling rows for embedding in a Form section, List or ScrollView. The host starts the model and remains responsible for durable application inbox processing; displaying the view never acknowledges records.

Omitting `displayName` (or passing an empty string) generates a name such as “Bright Avocado” once in the shared Rust session. Reopening the same private state directory with the same secure-store identity preserves it. Explicit names remain supported. Names are display labels, may collide, and grant no trust. Renaming can be implemented by awaiting model shutdown and closing the session, then reopening the same state/keys with a validated explicit name; the view accepts an optional `onRename` host callback.

```swift
LibreSyncNearbySyncView(model: model)
// Before the host closes or replaces its session:
await model.stopAndWait()
await session.close()
```

Discovery runs while the model is started; invitations open only after an explicit user action. Compatibility is separate from whether a device is currently offering a valid pairing code. Dismissing Add Device closes the local invitation and cancels outstanding pairing work. The UI distinguishes remote storage from the other app applying changes, and shows technical evidence under Details.

For physical iPhone–Mac testing, the [iOS demo](../../demos/NearbySyncIOS/README.md) uses the same shared host, data contract and pairing UI, including foreground/background lifecycle and protected local storage.

Try the [macOS notes demo](../../demos/NearbySyncMac/README.md) for a complete app with isolated profiles and durable receipt acknowledgement. From the repository root, run `scripts/run-nearby-sync-demo.sh --two` to build and launch two demo instances.

The notes convenience uses exactly the companion's public `notes_manifest`. A custom application chooses `policy: .records` and its own stable `LibreSyncManifest` app ID, namespace and `logical-records-v1` descriptors. This explicitly selects the reviewed pure logical LWW/tombstone policy. `momentum` selects the exact verified immutable Momentum policy. Other custom schemas are refused with `InvalidAdapter`; register their pure `ManagedAdapter` in Rust instead. Session owns storage; policies must not write an external database.

The existing `LibreSyncConnectView`, `LibreSyncDevicesView`, and `LibreSyncStatusView` remain available with one shared model. They include offline QR display/optional camera scanning, actual nearby friendly devices and six-digit codes, explicit Combine/Cancel, pause/resume, repair and data-preserving removal. `Samples/QuickstartApp` contains a SwiftUI application source and Info.plist. Copy it into an app target and add the extracted SDK package. The UI does not establish trust from Bonjour hints; core authentication checks pinned certificates, pairing version and the complete manifest.

For ordinary iOS discovery, declare `NSLocalNetworkUsageDescription` and `NSBonjourServices = ["_libresync._tcp"]`. Optional QR scanning also needs `NSCameraUsageDescription`. The SDK defaults to system `NetService` Bonjour on the actual Rust listener, with raw core multicast disabled. It needs no restricted multicast entitlement for that fixed declared service. Default listeners use IPv4; resolved AAAA hints are retained only for an explicitly configured IPv6 listener. Scoped IPv6 values use numeric scope IDs. IPv6 reachability is not claimed from the IPv4 native test. Raw BSD multicast is an advanced core opt-in requiring the restricted entitlement on physical iOS.

A permission timeout or an empty nearby list means `Unknown`; only actual platform policy evidence means `Denied`. Simulator tests cannot verify physical iOS Local Network privacy. Pause when backgrounded and resume/wake on foreground; iOS suspension limits background work. Await `model.stopAndWait()` and then `session.close()` when the host is finished.

```swift
let inbox = try await session.inbox()
// Persist records AND captured receipts, including every opaque 32-byte proof,
// in one durable app transaction. This sample fsyncs file and parent directory.
try await Task.detached { try LibreSyncInboxJournal.save(inbox, to: appFile) }.value
try await session.acknowledge(inbox) // only after successful durable app save
```

All blocking native work runs off the main thread. `events()` is a cancellable AsyncSequence; stopping observation does not cancel shared networking. Cancelling a connect/repair asks core to cancel current network operations. Explicit `cancel()` is session-wide for currently registered operations, and the scheduler may retry later; use pause for sustained suspension and close to fence new work. Concurrent close callers await the same cleanup. Events are snapshot-change notifications, with a 256-event newest buffer rather than a durable transaction log. Each observation gets an independent bounded native queue; stopping one subscriber preserves the others.

Managed state retains its secure-store account in the nonsecret `identity-ref.json` file beside `session.enc`. Keep both files with the app’s private state. Moving an iOS app container during an update preserves the reference, identity, peer trust and encrypted records; the host must also preserve its own durable note/database files and the same Keychain service/access group.

When upgrading a legacy installation that has no reference, `LibreSyncSession.open` first tries its old lookup. If the container already moved, a concrete `LibreSyncKeychainStore` can automatically recover by examining a bounded set of managed account names in that exact service. It accepts a candidate only after Rust authenticates the existing encrypted journal and validates its identity and app manifest, then writes a reference to that same account. No key is copied or regenerated, and locked/corrupt storage stops migration. Custom `LibreSyncSecureStore` implementations are not enumerated; upgrade once at the old path or provide an app-reviewed recovery strategy before relocating legacy state.

A complete state copy preserves device identity and must not be run alongside the original as an independent device. Downgrading a newly created portable installation or a relocated installation to an older SDK is unsupported. Uninstall/reinstall with lost app data or unavailable Keychain credentials is distinct from a state-preserving app update.

Keychain `not found` is distinct from locked, unavailable or corrupt storage. There is no plaintext fallback or silent identity regeneration. Errors use `LibreSyncSessionErrorCode`, with separate oversized/invalid record, incompatible version, revoked peer, group conflict and storage errors. Record bytes and proofs use exact base64 Codable `Data`; UInt64 counters retain their full precision.

`scripts/package-native-sdk.sh 0.8.0` builds and packages all five Apple architectures and both Android ABIs. The complete `LibreSyncSwift-0.8.0.zip` has a local XCFramework manifest, sources and tests and can be used as an extracted path dependency. The repository root manifest selects the immutable v0.8.0 release XCFramework ZIP with its exact checksum by default. Source builds explicitly select `LIBRESYNC_USE_LOCAL_XCFRAMEWORK=1` after building the XCFramework; the extracted SDK package uses its bundled local XCFramework.

After durably saving the complete coherent inbox, acknowledgement sends only its captured revision and exact source receipt map; record bytes stay in the saved app transaction and are not retransmitted in the acknowledgement request. This keeps a near-limit valid record aggregate acknowledgeable even with multiple proof-bearing source receipts. The C command accepts an optional legacy `records` field but does not use it. Cancellation before a queued Swift call starts prevents native work; cancellation after start interrupts current IO on a best-effort basis and is not transaction rollback.
