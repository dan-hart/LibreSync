# Nearby Notes for iOS

A native iPhone/iPad demo for pairing and syncing notes with the [macOS demo](../NearbySyncMac/README.md). It compiles the Mac demo’s existing `DemoModel.swift` and `DemoStorage.swift` and uses the same `io.libresync.NearbySyncDemo` app manifest, `notes` adapter and `logical-records-v1` schema. The native Notes and Nearby Sync tabs use the same Rust engine and SwiftUI pairing component.

## Build and install

Requires Xcode, rustup with the Apple targets, and the App Store Connect CLI (`asc`). Install XcodeGen if you want to regenerate the committed project after changing `project.yml`. Provisioning uses your own configured development team; the repository does not select a personal team.

From the repository root:

```sh
scripts/run-nearby-sync-ios-demo.sh --team YOURTEAMID --device dip17pm
```

The helper builds the current Apple SDK, resolves the explicitly named physical iOS device, development-signs the app, verifies the signature, installs it without uninstalling existing data, launches it, and records installed-app and running-process evidence. Xcode may update the development provisioning profile. The app appears as **Nearby Notes**. Unlock the phone if installation or launch requests it.

If the current source XCFramework is already built, add `--skip-sdk-build`. Use `--build-only` to stop before installation. Output is under `target/nearby-sync-ios/` or the supplied `CARGO_TARGET_DIR`; profiles, signing artifacts and app data are not committed.

```sh
xcodegen generate --spec demos/NearbySyncIOS/project.yml --project demos/NearbySyncIOS
scripts/build-apple-sdk.sh --debug
asc xcode build --project demos/NearbySyncIOS/NearbyNotesIOS.xcodeproj \
  --scheme NearbyNotesIOS --destination 'generic/platform=iOS Simulator' \
  --no-code-signing --output json
```

The target supports iOS 17 or newer. Device builds require automatic signing and a development certificate/provisioning profile that includes the selected phone. No App Store Connect app record, TestFlight upload, cloud service, iCloud entitlement or restricted multicast entitlement is needed.

## Pair the iPhone and Mac

1. Open Nearby Notes on both devices and keep both apps visible on the same local network. Allow **Local Network** when requested. Camera access is optional; six-digit pairing works without it.
2. On the Mac, open **Nearby Sync → Add Device → Show six-digit code**.
3. On iPhone, open **Nearby Sync → Add Device**, choose the Mac’s friendly name, and enter its code. Choose the intended profile if two Mac demo windows are running.
4. If both devices already have notes, review and approve the shared-data preview on each device before expecting sync completion.
5. Write and save a note on iPhone, verify it on Mac, then save an edit on Mac and verify it on iPhone. Check the connected-device status for application-confirmed completion.
6. Background and reopen the iPhone app, then verify the same name, notes and paired device remain. Foregrounding resumes sync; this local-only demo does not promise continuous iOS background networking.

An empty discovery list is not evidence of denied permission. Check that both apps are open and on the same network; use Connection help and details for actual permission/error evidence. Guest Wi-Fi isolation may prevent devices from reaching each other.

## Storage and verification

The iPhone has its own sandbox and Keychain identity even when its demo profile has the same name as a Mac profile. The app validates and durably saves one coherent notes-and-receipts inbox before acknowledging Applied. Its iOS state root is excluded from device backups; plaintext note files use complete file protection. The Rust session journal is encrypted, and keys use Keychain with no plaintext fallback. Device names are public discovery labels and never establish trust.

The shared host tests cover backgrounding during open and rename, same-session pause/resume, real two-host Bonjour/code pairing, durable application acknowledgement, and persisted notes/identity/trust after reopen. Simulator compilation and these host tests do not establish physical Local Network privacy or successful phone–Mac pairing. Build, signed install, launch, discovery, pairing and bidirectional saved-note delivery are separate acceptance checks.
