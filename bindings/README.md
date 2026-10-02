# Native bindings

New applications use managed Session through [Swift async/SwiftUI](swift/README.md), [Android suspend/Flow/Compose](kotlin/README.md), or [managed C ABI v1](../docs/MANAGED-C-ABI.md). Install exact v0.7.0 release assets with the [SDK guide](../docs/SDK.md).

Session owns listener, discovery, authenticated QR/code pairing, retries and typed status. Platform secure storage fails closed. Apps commit their coherent inbox records and exact proof-bearing receipts durably before acknowledging Applied; a companion reports Stored until an app processes data.

`include/` contains the public C header; `managed-v1.schema.json` defines typed managed messages. Swift includes Keychain/system Bonjour and the notes sample. Android includes actual ARM64/x86_64 16 KiB JNI, Keystore, API37 permission handling and the notes sample. Optional components supply Connect/Devices/Status UI.

Legacy C/Swift Engine surfaces remain separate. Historical Kotlin raw Engine/backup methods had no JNI implementation and now explicitly refuse with migration guidance. Old SQLite mapping samples are low-level examples, not managed transactional policies.

Apple Info.plist Bonjour/Local Network declarations and optional camera usage are required; physical privacy/camera acceptance remains unverified. Android foreground MulticastLock/runtime permission and both platforms' background restrictions still apply. See platform READMEs before integrating.
