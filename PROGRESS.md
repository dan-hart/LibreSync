# Progress log

## Summary
LibreSync has a working device-to-device MVP for two CLI instances on a LAN. Devices can discover each other, pair with consent, and sync a single JSON file with deterministic conflict resolution.

## Completed
- Rust workspace with core library and CLI.
- Device identity model (device/app/user IDs) with per-app trust.
- LAN discovery via mDNS.
- Pairing flow (trust only) and sync flow (data exchange).
- Device listener for inbound connections.
- CLI commands: init, select, discover, pair, listen, sync, status.
- Status output with last-seen timestamps.
- Listener runs in the background by default with PID/log output.
- Discovery filters out the local device from selection lists.
- Verbose error output available via `--verbose`.
- Architecture and CLI docs in `docs/`.
- Test suite covering core and CLI flows.

## Current MVP behavior
- Two devices on the same LAN can run `libresync`.
- Devices auto-discover via mDNS and can be selected interactively.
- Pairing requires consent on both devices and adds to the allowlist.
- Sync exchanges a single JSON file and applies last-writer-wins via Lamport clocks.

## Known gaps
- No cryptographic identity or encrypted transport yet (identity is string-only).
- Single-file sync only (one JSON file mapped to the `file` key).
- No background watchers or continuous sync.

## Next steps
- Add cryptographic device/app keys and signed handshakes.
- Encrypt transport (Noise or TLS).
- Expand to multi-file or structured adapters.
- Add optional file watching for near-real-time sync.

## Tests
- `cargo test` covers core and CLI behavior.
