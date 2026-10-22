# Nearby Notes for macOS

A native SwiftUI app that uses the public LibreSync Swift package and real Rust sessions. Write notes in **Notes**, then open **Nearby Sync** to connect another installation. The app uses macOS sidebar, window, sheet, and keyboard conventions; Command-S saves the current note.

## Build and run

From the repository root on a Mac with Xcode command-line tools, Rust (through rustup), and the native Apple Rust target installed:

```sh
scripts/run-nearby-sync-demo.sh --two
```

This builds and ad-hoc signs `target/nearby-sync-demo/Nearby Notes.app`, then launches two separate processes using `primary` and `secondary` profiles. Both windows stay connected while unfocused. No Xcode project or universal/iOS build is required. `CARGO_TARGET_DIR` overrides the output location; the script prints all artifact paths.

```sh
scripts/run-nearby-sync-demo.sh --build-only
scripts/run-nearby-sync-demo.sh --profile primary
```

The script stages the current SDK and demo sources under its output directory, preserving `.package(path: "../../bindings/swift")`. It creates a native Mac XCFramework there and leaves any existing full SDK XCFramework intact. The script selects rustup's compiler by default, sets the macOS deployment target to 12, and respects an explicit `RUSTC`. Build output uses the native architecture only.

Run the demo tests with the exact command printed by the build script. Tests verify profile validation, journal failure before acknowledgement, and durable records/receipt readback at acknowledgement time. The two-host integration test exercises actual Bonjour discovery, code pairing, app note application, Applied receipts, authenticated rename, and restart persistence using temporary state and ephemeral test keys.

## Try two profiles

1. Save a note in each window.
2. Open **Nearby Sync** in both windows. Choose **Add Device** and show a code on one window.
3. Select that device in the other window and enter its six-digit code. Review the merge if requested.
4. Check **Notes** in both windows. Save another note, then quit and reopen both profiles to check persistence.

Local Network permission may be requested by macOS. Allow access for discovery. Check the SDK's permission/recovery guidance if devices do not appear. Two profiles exercise actual Bonjour discovery and authenticated sessions on one Mac; two physical Macs are still needed to validate real network and permission behavior.

For disposable GUI validation, launch `--profile smoke-primary` and `--profile smoke-secondary`. Profile names accept only 1–32 lowercase ASCII letters, digits, or hyphens. All profiles share the same app manifest and schema; they have separate identities, trust, keys, journals, and note databases. The window title and sidebar identify the active profile. Launching the same profile twice fails visibly because the session owns its state lease.

## Data and trust

App data lives in `~/Library/Application Support/io.libresync.NearbySyncDemo/<profile>/`, with a private profile directory and journal file. Keys use macOS Keychain service `io.libresync.NearbySyncDemo.<profile>`; there is no plaintext fallback. Resetting only files while retaining mismatched keys is not a supported reset flow. Do not remove state while the corresponding process is running.

The fixed app ID is `io.libresync.NearbySyncDemo`. The custom records adapter is `notes`, with namespace `notes` and schema `logical-records-v1`. The app saves UUID-addressed JSON notes through `session.set`. Inbox application validates notes, durably saves the coherent records and exact receipts with `LibreSyncInboxJournal`, and only then acknowledges the captured inbox. Persistence failures remain visible and prevent acknowledgement. The app's plaintext note journal is private application data; session transport/journal encryption and Keychain protect separate parts of the system.

Device names are generated once by Rust and persist across restarts. Rename stops discovery and actions, awaits session close, then reopens the same state with an explicit display name. Names help recognition; authenticated pairing establishes trust. Quit awaits in-flight application work, the SDK's stop lifecycle, and session close. The demo never logs pairing secrets and stores no example user data in the repository.
