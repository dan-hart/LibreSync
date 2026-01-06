# Bindings

This directory contains starter SDKs for Swift and Kotlin that wrap the C ABI in `libresync-ffi`.

- `include/` holds the C header used by the bindings.
- `swift/` provides a Swift Package wrapper and sample.
- `kotlin/` provides a JNI-style wrapper and sample.

These bindings are intentionally minimal and intended to evolve alongside the stable API surface.

## Platform notes
- Apple platforms must declare Local Network permissions in Info.plist for discovery.
- Android requires multicast permissions and a `MulticastLock` for mDNS.
- Store app keys and device keys in platform secure storage (Keychain/Keystore).
