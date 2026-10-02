# Install v0.7.0 native SDKs

Swift Git consumers select `https://github.com/dan-hart/LibreSync.git` at exact tag `v0.7.0`; the root manifest downloads the immutable release XCFramework ZIP with its exact checksum. Alternatively extract `LibreSyncSwift-0.7.0.zip` and use its local package. Source builds and CI explicitly set `LIBRESYNC_USE_LOCAL_XCFRAMEWORK=1` after `scripts/build-apple-sdk.sh`.

Android consumers extract `LibreSyncAndroid-0.7.0-maven.zip`, add that directory as a Maven repository, and depend on `io.libresync:libresync:0.7.0` plus optional `io.libresync:libresync-compose:0.7.0`. Both AARs include the license; public AndroidX/Kotlin dependencies require network or a populated cache. See [Android setup](../bindings/kotlin/README.md) and [Swift setup](../bindings/swift/README.md).

Use managed ABI v1, Swift async and Kotlin suspend/Flow Session APIs. Legacy C/Swift Engine APIs remain. Historical Kotlin raw Engine/backup APIs had no JNI implementation and now explicitly refuse; migrate to Session. Native custom policies are limited to the reviewed notes/Momentum and transactional `logical-records-v1` contracts. Commit coherent inbox records and exact receipts durably before acknowledging; Stored is not app Applied. Events are bounded notifications, not a durable transaction log.

## Integrate the managed session

1. Choose a stable app ID and transactional adapter/schema contract. Use the exact notes/Momentum policies or reviewed generic `logical-records-v1`; arbitrary native policy callbacks are unsupported. Other domain policies use a pure Rust `ManagedAdapter`.
2. Open Session with a private directory and Keychain/Keystore. Identity creation happens only when absent; unavailable/corrupt secure storage fails without regenerating identity.
3. Start and observe typed snapshots/events. Native operations dispatch IO off the main thread. Show compatible named devices and use authenticated expiring QR/code invitations; never log invitation secrets.
4. Import into an empty app, or obtain a populated bootstrap preview and ask Combine/Cancel. A stale preview cannot authorize a changed candidate. Preserve the pre-merge recovery model.
5. Publish edits with stable IDs/clocks and tombstones. Save a coherent application inbox **including its exact receipt proofs** in one durable app transaction, then acknowledge that captured inbox. A failed app save must not acknowledge. Stored proves peer storage; Applied proves this application-processing boundary.
6. Pause/resume/wake with foreground lifecycle and close when finished. Repair requires an explicit pinned invitation; remove revokes future exchanges while preserving local data and cannot erase remote copies.

Events are independent bounded notifications, not a durable transaction log. Cancellation is best effort after work starts and does not roll back committed changes; pause/close sustain suspension. Numeric counters and opaque receipt proofs must be preserved exactly. See [the managed contract](MANAGED-SESSION.md) and [C ABI ownership/schema](MANAGED-C-ABI.md).

## Components and working samples

Swift exposes async Session, an observable model, Keychain and fixed-service system Bonjour. Optional SwiftUI Connect/Devices/Status views provide QR/code pairing, merge consent, troubleshooting and device controls; the notes sample durably saves incoming notes. Android exposes suspend/Flow Session, Keystore, foreground discovery/multicast lifecycle, API37 permission support and optional Compose components with QR/image import. Use [Swift notes](../bindings/swift/Samples/QuickstartApp) and [Android notes](../bindings/kotlin/sample) as the runnable app integration references.

The configurable records policy does not synchronize arbitrary app databases automatically. Session's encrypted records and the app's own database are separate stores; apps own their durable-save and merge choices. Companion recovery exports require explicit consent because they contain unencrypted app data.

## Platform constraints

Apple apps declare `NSLocalNetworkUsageDescription` and fixed `NSBonjourServices` `_libresync._tcp`; optional camera scanning requires its camera usage description. The Swift SDK uses system Bonjour; advanced raw Rust multicast on iOS needs the restricted multicast entitlement. A timeout/empty discovery list remains Unknown unless actual platform policy supplies denial evidence. Simulator cannot prove physical iOS Local Network privacy.

Android apps declare required network/multicast permissions; API37 with targetSDK37 needs runtime `ACCESS_LOCAL_NETWORK` handling. MulticastLock follows foreground lifecycle. iOS suspension and Android background restrictions prevent a perpetual mobile background daemon; resume/wake when active. LAN firewalls/client isolation can still prevent reachability. Current managed listener defaults to IPv4; no tested IPv6 transport claim.

Local runtime acceptance uses macOS native sessions and an ARM64 API37/16 KiB emulator. CI separately requires actual x86_64 API37/16 KiB evidence. All Apple slices/generic iOS compilation and both Android JNI ABIs are built. Physical privacy/camera and physical Android hardware remain unverified. Swift5.9 mode is supported; strict Swift6 consumer compatibility is not claimed.

## Legacy compatibility

The low-level C Engine ABI version2 and Swift `LibreSyncEngine`/event pump remain for existing integrations. They require manual listener, linking, adapter and sync orchestration; their whole-file/SQLite mapping and backup APIs do not become managed transactional Session policies. See [legacy API examples](API.md) and the [C header](../bindings/include/libresync.h).

Historical Kotlin raw Engine/backup declarations never had JNI implementations. They are deprecated and explicitly throw migration guidance. Only supported legacy ABI/key-generation helpers have actual JNI symbols; use managed Session for Android. Old Room/SwiftData mapping examples describe that separate low-level boundary and are not managed notes samples.

## Build and distribute

`scripts/package-native-sdk.sh 0.7.0` builds five Apple slices and both Android ABIs, then emits complete Swift/XCFramework ZIPs, core/Compose AARs, a Maven repository ZIP, C header/schema and SHA256SUMS. Every public archive/AAR includes AGPL licensing. Native source/compiler provenance and fresh extracted consumer checks belong in release evidence. Follow [the release checklist](RELEASING.md); root Git-SPM remote resolution is verified after exact release assets are published.
