# Packaging on macOS

## Building the FFI library for Swift

`libresync-ffi` is a `staticlib`/`cdylib` crate. For SwiftPM consumption build
a universal static library and wrap it in an XCFramework:

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
scripts/build-macos-universal.sh          # or --debug
cd bindings/swift && swift build && swift test
```

The script runs `cargo build --target` for both architectures, `lipo`s the
archives into `target/universal-apple-darwin/release/liblibresync_ffi.a`, and
creates `bindings/swift/LibreSyncFFI.xcframework`, which `Package.swift`
references as a `binaryTarget`. `swift test` runs `Tests/LibreSyncTests`
against the real library (engine creation, listener, event descriptor, async
sync). CI does this on `macos-latest`.

To consume the package from another repository, publish the XCFramework as a
zip and replace the `path:` binary target with `url:` + `checksum:`
(`swift package compute-checksum LibreSyncFFI.xcframework.zip`).

The Rust static library depends on `CoreServices` (FSEvents, used by the
`notify` crate), `CoreFoundation` and `Security`; `Package.swift` declares
them as `linkedFramework`s, so apps do not have to.

## App Sandbox and entitlements

LibreSync opens a TCP listener and outgoing TCP connections, and joins the
mDNS multicast group. In a sandboxed app that requires:

| Entitlement | Why |
| --- | --- |
| `com.apple.security.app-sandbox` | Sandbox itself (Mac App Store / hardened runtime). |
| `com.apple.security.network.client` | Outgoing sync and link connections, mDNS queries. |
| `com.apple.security.network.server` | The inbound listener (`Engine::start_listening`) and mDNS responder socket. |

Example `LibreSyncApp.entitlements`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.app-sandbox</key><true/>
    <key>com.apple.security.network.client</key><true/>
    <key>com.apple.security.network.server</key><true/>
    <!-- Only if adapters read user-chosen files outside the container -->
    <key>com.apple.security.files.user-selected.read-write</key><true/>
</dict>
</plist>
```

`Info.plist` additionally needs the Local Network privacy keys (macOS 15+
prompts for them):

```xml
<key>NSLocalNetworkUsageDescription</key>
<string>Finds your other devices on this network to sync directly with them.</string>
<key>NSBonjourServices</key>
<array>
    <string>_libresync._tcp</string>
</array>
```

Without `network.server` the listener bind fails with `EPERM`; without the
Local Network permission mDNS queries return nothing and inbound connections
are silently dropped. `LibreSyncLocalNetworkPermission` in the Swift package
triggers the prompt early. The engine's own sockets (the event wake-up pipe,
the loopback wake connection used to stop the listener) do not need extra
entitlements.

## mDNS: `mdns-sd` and Bonjour

macOS runs `mDNSResponder` (Bonjour), which owns UDP 5353. The `mdns-sd` crate
binds its own socket to 5353 with `SO_REUSEPORT`/`SO_REUSEADDR` and joins the
multicast group, so both coexist: Bonjour keeps answering for system services,
`mdns-sd` answers for `_libresync._tcp.local.`. There is no port conflict as
long as the process may bind (see entitlements above). The listener itself
uses TCP 52345 by default, which Bonjour never binds.

If you prefer the system responder, advertise with `NetService` /
`NWListener(service:)` using the same service type and TXT keys documented in
`docs/PROTOCOL.md` (`app_id`, `device_id`, `user_id`) and keep
`register_mdns` off; the Rust browser (`browse_mdns`) reads Bonjour-published
records fine.

## Key storage: Keychain

Store the app key and device identity in the Keychain, never in a plain
file. Two paths exist:

- **Swift apps**: `LibreSyncKeychain` / `LibreSyncKeyManager` in
  `bindings/swift` use the Security framework (`SecItemAdd` /
  `SecItemCopyMatching`, generic passwords, service `LibreSync` by default).
  This works inside App Sandbox with no extra entitlement; enable
  `keychain-access-groups` only when several of your apps share keys.
- **Rust daemons / CLI**: `libresync::SecurityCliKeyStore` drives
  `/usr/bin/security` (`add-generic-password -U`,
  `find-generic-password -w`) against the login keychain. It stores items
  under the same generic-password shape (service = app id, account = item
  name), so the Swift helper can read them when configured with the same
  service. Items are base64 in the password field.

Both implement the `KeyStore` shape (`get` / `set` / `delete` by name);
`KeyStoreExt` adds `load_or_create_app_key` and
`load_or_create_device_keys`. `platform_key_store` picks the Keychain backend
on macOS and the Secret Service backend on Linux, falling back to
`FileKeyStore`. The Linux mirror is described in `docs/PACKAGING-LINUX.md`.

## launchd user agent

`contrib/launchd/com.codedbydan.libresync.alwayson.plist` runs the always-on
daemon as a user agent; install steps are in `contrib/README.md`.
