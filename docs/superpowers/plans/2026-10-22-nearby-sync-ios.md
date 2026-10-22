# iOS Nearby Notes implementation plan

**Goal:** Build a native iPhone notes demo compatible with the macOS demo and install/launch it on dip17pm.

**Architecture:** Compile the existing demo model/storage into the iOS app rather than copying sync or note persistence logic. Keep the exact app manifest and logical record contract. Add native iOS notes/navigation/rename UI and reuse LibreSyncNearbySyncView. Use Keychain, system Bonjour and explicit code/QR trust; pause on background and resume on foreground while preserving the session.

## Tasks

- [x] Make DemoModel portable: conditional AppKit delegate, mobile device kind on iOS, serialized pause/resume lifecycle that cannot race startup/rename/shutdown. Add host regression tests before behavior changes; retain all macOS tests.
- [x] Add demos/NearbySyncIOS with an XcodeGen spec, generated Xcode project, iOS-native SwiftUI notes and Nearby Sync tabs, note editor, rename, and visible error/recovery. Compile existing DemoModel.swift and DemoStorage.swift sources. Use the same io.libresync.NearbySyncDemo manifest and notes schema.
- [x] Declare Local Network, fixed _libresync._tcp Bonjour, optional QR camera purpose, and native Keychain access. No cloud or restricted multicast entitlement. Exclude iOS state from device backups and apply complete file protection to plaintext note inbox data before acknowledgement. Verify active/background/relaunch retains notes, identity and trust. Add repeatable local Apple SDK/device build instructions and CHANGELOG entry.
- [x] Build current Rust FFI for Apple architectures with rustup compiler and correct deployment targets; build iOS for simulator and dip17pm using configured development signing. Keep provisioning artifacts/build outputs ignored.
- [x] Run Swift/demo tests and independent specification/code review; preserve macOS behavior and saved data.
- [ ] Verify signed app, install on the exact live dip17pm, launch, and read back app/process evidence. Report phone unlock/privacy requirements only if encountered. Open a compatible Mac demo profile for user testing; distinguish installation from actual physical pairing/notes acceptance.

## Verification

All five current Rust Apple architectures built. The iOS simulator and personal-team development-signed dip17pm builds passed; the provisioning profile includes the exact phone. All nine shared-host/demo tests passed, including real Bonjour/code pairing, durable Applied receipts, foreground/background transitions, rename and reopened note/identity/trust preservation. The rebuilt Mac demo package also passed all nine tests. Source, deployment helper and privacy/lifecycle behavior passed independent specification and code-quality review.

The installed-app readback confirms Nearby Notes 1.0.0 (build 1), bundle `io.libresync.NearbySyncDemo`, on dip17pm. Initial launch was explicitly refused because the phone was locked. Launch/process verification awaits unlock; successful physical iPhone–Mac pairing and bidirectional note delivery remain manual acceptance checks.
