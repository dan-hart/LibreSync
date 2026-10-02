# Android managed SDK

The Android library includes actual JNI symbols, typed coroutine `Session` APIs, cancellable `Flow` observation, Android Keystore and lifecycle/permission helpers. The optional `compose` library supplies Connect, Devices and Status, including offline ZXing QR display and camera/photo consumption, actual friendly nearby devices/code entry and explicit bootstrap consent.

Set `ANDROID_HOME`, `JAVA_HOME` (JDK 17), and optionally `ANDROID_NDK_HOME`, then run `scripts/build-android-sdk.sh`. It builds arm64-v8a and x86_64 shared libraries, verifies 16-KiB ELF LOAD alignment, and publishes both AARs with dependency metadata to `bindings/kotlin/build/repository`. Gradle 9.3.1 and AGP 9.1.1 support API 37.0. Use the supplied wrapper. Build the runnable offline notes quickstart with `bindings/kotlin/gradlew -p bindings/kotlin :sample:assembleDebug`.

Use the extracted Maven repository (or publish that directory to your own artifact repository):

```kotlin
repositories { maven { url = uri("/path/to/repository") }; google(); mavenCentral() }
dependencies {
    implementation("io.libresync:libresync:0.7.0")
    implementation("io.libresync:libresync-compose:0.7.0") // optional
}
```

```kotlin
val session = Session.open(
    SessionConfiguration.notes(File(context.filesDir, "notes"), "My phone"),
    AndroidKeystoreStorage(context, "io.libresync.Notes"))
val resources = SessionLifecycle(context, session)
lifecycle.addObserver(resources)
```

The notes policy exactly matches the always-on companion catalog. For your own stable app ID and adapter namespace, choose `policy = "records"`, supply an `AppManifest`, and use only transactional `logical-records-v1` descriptors. The reviewed pure logical LWW policy and tombstones apply. `momentum` selects the exact verified immutable Momentum contract; unsupported custom schema/policy choices fail with `InvalidAdapter`. Arbitrary schemas do not silently become byte-record LWW. Register a pure Rust `ManagedAdapter` for other domain policies.

```kotlin
val inbox = session.inbox()
withContext(Dispatchers.IO) { InboxJournal.save(appFile, inbox) }
session.acknowledge(inbox) // only after the entire app transaction is durable
```

Save records and the exact captured source receipts, including all opaque 32-byte proofs, together. A failed app save must not advance Applied. `InboxJournal` demonstrates file and parent-directory fsync; replace it with your own durable database transaction. Session owns its encrypted journal; an app's database is not part of the pure adapter transaction.

Android Keystore protects one AES-GCM key per service namespace. Ciphertext files live in app-private no-backup storage, use per-item AAD, and are atomically fsynced. A process-wide mutex and kernel file lease serialize alias creation and writes across instances and processes; reads reload files under that lease. Only actual ENOENT means absent. Missing keys beside ciphertext, locked storage, malformed ciphertext, and I/O failures throw; no plaintext fallback or silent regeneration exists.

The merged manifest declares INTERNET, ACCESS_WIFI_STATE, CHANGE_WIFI_MULTICAST_STATE and ACCESS_LOCAL_NETWORK. On API 37 with target SDK 37, request ACCESS_LOCAL_NETWORK at runtime. Older/opt-in platforms report Unknown when evidence is unavailable. `SessionLifecycle` acquires/releases MulticastLock with foreground/background transitions and exposes lifecycle failures as a typed StateFlow. Respect Android background limits; resume/wake on foreground rather than promising a perpetual mobile daemon.

`close()` invalidates the handle immediately and schedules cleanup. `closeAndJoin()` joins the same cleanup even after prior or concurrent close and propagates failures. Cancelled Flow observation only stops bounded polling. Cancelling connect/repair asks core to cancel currently registered network operations; explicit `cancel()` is session-wide and the scheduler may retry, so pause for sustained suspension. Each Flow collector has an independent bounded native subscription; cancelling one preserves other observers.

The historical Kotlin Engine wrapper declared C names as JNI functions without implementing JNI. Its raw Engine/backup methods are now explicitly deprecated and throw a migration message. ABI/key-generator functions have real JNI support. Legacy C and Swift Engine APIs remain interoperable; new Android applications use managed Session.

Native tests run with `ANDROID_SERIAL=<owned-device> ./gradlew connectedDebugAndroidTest`. API-37/16-KiB instrumented tests cover actual JNI, duplicate lease rejection, typed pairing/bootstrap, proof-bearing durable inbox acknowledgements, Flow cancellation, close/reopen and parallel Keystore instances. This does not establish physical camera accuracy or a human usability result.

Build distributable Maven metadata and both AARs with `scripts/build-android-sdk.sh`. Package all native SDK assets with `scripts/package-native-sdk.sh 0.7.0`. Extract `LibreSyncAndroid-0.7.0-maven.zip` and add its directory as a Gradle Maven repository, then depend on `io.libresync:libresync:0.7.0` and optionally `io.libresync:libresync-compose:0.7.0`; metadata supplies public coroutine, serialization, lifecycle and Compose dependencies. Normal AndroidX/Kotlin dependencies must already be cached for a fully offline build.

After durably saving the complete coherent inbox, acknowledgement sends only its captured revision and exact source receipt map; record bytes stay in the saved app transaction and are not retransmitted in the acknowledgement request. This keeps a near-limit valid record aggregate acknowledgeable even with multiple proof-bearing source receipts. The C command accepts an optional legacy `records` field but does not use it. Cancellation before a queued Swift call starts prevents native work; cancellation after start interrupts current IO on a best-effort basis and is not transaction rollback.
