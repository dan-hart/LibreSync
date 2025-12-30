# Progress log

## Summary
LibreSync has a working device-to-device MVP for two CLI instances on a LAN. Devices can discover each other, pair with consent, and refresh a single JSON file with deterministic conflict resolution.

## Completed
- Rust workspace with core library and CLI.
- Device identity model (device/app/user IDs) with per-app trust.
- LAN discovery via mDNS.
- Pairing flow (trust only) and refresh flow (data exchange).
- Device listener for inbound connections.
- CLI commands: init, select, discover, pair, listen, refresh, watch, status.
- Status output with last-seen timestamps.
- Listener runs in the background by default with PID/log output.
- Discovery filters out the local device from selection lists.
- Verbose error output available via `--verbose`.
- Architecture and CLI docs in `docs/`.
- Test suite covering core and CLI flows.
- SDK-first `Engine` API with adapters and device discovery/pairing.
- CLI dogfoods the `Engine` for listen, discover, pair, refresh, and watch.
- Frankly Tauri demo app with SQLite-backed todos and LAN device linking.

## Current MVP behavior
- Two devices on the same LAN can run `libresync`.
- Devices auto-discover via mDNS and can be selected interactively.
- Pairing requires consent on both devices and adds to the allowlist.
- Sync exchanges a single JSON file and applies last-writer-wins via Lamport clocks.

## Known gaps
- Device keys are self-signed and stored locally; no rotation/revocation or shared trust roots yet.
- Pairing uses fingerprint allowlisting (TOFU) without out-of-band verification prompts.
- Single-file refresh only (one JSON file mapped to the `file` key).
- No background watchers or continuous refresh.

## Next steps
- Add key rotation/export, fingerprint display UX, and re-pairing flows.
- Expand to multi-file or structured adapters.
- Add optional file watching for near-real-time refresh.
- Plan FFI boundary + Swift/Kotlin wrapper approach.

## Tests
- `cargo test` covers core and CLI behavior.
