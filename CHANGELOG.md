# Changelog

All notable changes to LibreSync are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); release notes with
narrative context live in `RELEASES.md`.

## [Unreleased]

### Security
- `read_message` now caps a single wire message at `MAX_MESSAGE_BYTES`
  (256 MiB) so an unlinked peer cannot force unbounded memory use before the
  `Hello` check. `read_message_with_limit` exposes the bound for callers.
- SQLite logical adapter quotes all developer-supplied table and column
  identifiers when building SQL.
- Bumped `crossbeam-epoch` to 0.9.21 (RUSTSEC-2026-0204) and `spin` to 0.9.9
  (previous version yanked).

### Changed
- `SECURITY.md` points at GitHub private vulnerability reporting.
- Added `CODE_OF_CONDUCT.md`, issue and pull request templates, and crate
  metadata (`repository`, `readme`, `keywords`, `categories`).
- Removed Homebrew as a distribution channel; install the CLI with
  `cargo install --git ... --tag vX.Y.Z libresync-cli` or from a checkout.
- Removed internal planning notes (`PROGRESS.md`, `docs/plans/`, `research/`);
  status lives in `CHANGELOG.md` and `RELEASES.md`.
- Test fixtures use TEST-NET-2 documentation addresses instead of CGNAT ranges.
- CI uses `actions/checkout@v5`; the CLI binary no longer collides with the
  library in `cargo doc` output.

## [0.6.0] - 2026-09-10

### Fixed
- **Merge policies now run on the sync path.** Inbound entries are routed to
  the registered adapter that owns their key (`DataAdapter::owns_key`) and
  applied through `apply_entries`, so `LogicalAdapter` merge policies
  (`SetUnion`, `ListAppend`, `Counter`, `Custom`, field-level LWW) apply during
  `Engine::sync_now` and on the listener, not only on backup restore. Entries no
  adapter owns keep whole-entry last-writer-wins. (`AdapterRouter`,
  `InboundApplier`, `LwwApplier`)
- Field-level merges that add content from an *older* record are no longer
  dropped by the whole-entry clock check (`RecordState::upsert`,
  `State::upsert_entry`).
- The listener loads registered adapters into the state before answering and
  writes merged data back afterwards, so a GUI app does not need a polling
  `watch` for inbound syncs to reach its store.

### Added
- **Per-field clocks** on logical records (`SyncRecord::field_clocks`, derived
  automatically on write). Two devices editing different fields of the same
  record concurrently both keep their edit. Test:
  `tests/two_device.rs::concurrent_edits_to_different_fields_survive_on_both_devices`.
- **Delta sync.** `Message::SnapshotSince { clock, epoch }` and
  `Message::Delta { .. }` (protocol version 2) exchange only entries applied
  after the peer's last-seen cursor, in both directions on one connection. The
  first sync, an unknown cursor, or a peer whose state was recreated (new
  `epoch`) fall back to a full snapshot. Version 1 peers keep working through
  the legacy push/pull path. `State` tracks apply sequences, origins and
  per-peer cursors (`PeerCursor`); old state files migrate on load. Test:
  `tests/two_device.rs::delta_sync_traffic_is_proportional_to_the_edit`.
- `SyncStats` (entries and TLS bytes per direction, full-snapshot flag,
  negotiated protocol version) returned by `Engine::sync_now_with_stats`,
  `Engine::sync`, `sync_with_device_using`, and emitted as `Event::SyncStats`.
- `MergePolicy::AppendOnly` for op-log fields: grow-only union that never drops
  items. `docs/LOGICAL.md` documents both event-log shapes. Test:
  `tests/two_device.rs::append_only_event_log_loses_nothing_under_concurrent_appends`.
- `Event::InboundSync` reports merges performed by the local listener.

### Security
- **Fingerprint pinning after first use.** Linking stays trust-on-first-use.
  Afterwards a linked device whose leaf certificate does not match the pinned
  fingerprint is rejected on both sides (`Error::FingerprintMismatch`) and
  `Event::FingerprintChanged` is emitted so the app can ask the user before
  re-linking. `SyncRequest::for_device` / `SyncOptions::with_expected_fingerprint`
  pin the fingerprint at the TLS layer so the handshake fails before any
  application data is sent. The TLS server name is the expected device id when
  known instead of a hard-coded value.
- Upgraded rustls 0.21 to 0.23 (ring provider); the `dangerous_configuration`
  feature is gone. Certificate verification now also checks handshake
  signatures with the provider's algorithms.
- The daemon logs fingerprint changes and never re-pins automatically; the CLI
  prints a re-link hint on mismatch.

### GUI embedding
- `Engine::spawn()` returns a `BackgroundEngine` that owns a thread and runs
  commands (`try_sync_now`, `try_sync`, `try_request_link`, `try_discover`,
  `try_start_listening`, `try_stop_listening`, `try_save_state`, `run`) without
  blocking the caller; each command gets a `Ticket` and reports
  `Event::TaskFinished`. `cancel(ticket)` aborts a running or queued command.
- `CancelToken` for blocking calls (`SyncRequest::with_cancel`,
  `SyncOptions::with_cancel`, `Engine::request_link_cancellable`); cancelling
  shuts the socket down so a stalled peer does not block until the timeout.
- `EventStream::raw_fd()` (readable while events are queued, for
  `g_unix_fd_add` / `DispatchSource`) and `EventStream::set_waker()`;
  `recv_timeout`, `len`, `is_empty`.
- FFI (ABI 2): `libresync_engine_event_next` / `event_wait` / `event_fd`,
  `libresync_engine_sync_async` + `libresync_engine_cancel`; device JSON
  carries `fingerprint`. Swift package: `LibreSyncEvent`, `LibreSyncEventPump`
  (DispatchSource on the event descriptor), `syncAsync`, `cancel`.
- `docs/API.md` documents the glib (thread + channel / fd) and Swift
  (DispatchQueue) integration patterns and the 0.6 migration.

### Packaging (Linux)
- `docs/PACKAGING-LINUX.md`: Flatpak `finish-args` (`--share=network`
  required; optional `--filesystem=/var/run/tailscale`), firewalld and ufw
  commands for mDNS (UDP 5353) and the listener port (TCP 52345), and an
  ignored mDNS round-trip test to verify a host.
- Tailscale discovery degrades explicitly: the CLI is probed once per process,
  a single `log::info!` line reports why it is disabled, and no further
  `tailscale status` attempts are made (`tailscale_cli_state()`,
  `reset_tailscale_probe()`).
- Feature `tailscale-local-api`: read peers from the Tailscale local API Unix
  socket (`/var/run/tailscale/tailscaled.sock`, override with
  `LIBRESYNC_TAILSCALE_SOCKET`) when the CLI is unavailable, e.g. in a Flatpak
  sandbox.
- `contrib/systemd/libresync-alwayson-daemon.service` (user unit,
  `Restart=on-failure`, hardened) and `contrib/README.md` with enable steps;
  `contrib/flatpak/` manifest fragment.
- New dependency: `log` (facade only).

### Packaging (macOS)
- `KeyStore` trait with `MemoryKeyStore`, `FileKeyStore`,
  `SecretToolKeyStore` (Linux Secret Service via `secret-tool`) and
  `SecurityCliKeyStore` (macOS login Keychain via `/usr/bin/security`), plus
  `KeyStoreExt::load_or_create_app_key` / `load_or_create_device_keys` and
  `platform_key_store`. Mirrors the Swift `LibreSyncKeychain` helper.
- `scripts/build-macos-universal.sh` builds the arm64 + x86_64 static
  `libresync-ffi` and wraps it in `LibreSyncFFI.xcframework`;
  `bindings/swift/Package.swift` consumes it as a binary target and gained an
  XCTest target that exercises the engine, listener, event descriptor and
  async sync.
- `contrib/launchd/com.codedbydan.libresync.alwayson.plist` user agent
  (restart on failure only).
- `docs/PACKAGING-MACOS.md`: App Sandbox entitlements
  (`network.client` / `network.server`), Local Network keys, `mdns-sd` and
  Bonjour coexistence, Keychain paths.

### Documentation and CI
- `docs/PROTOCOL.md`: message types, framing, link and sync handshakes (v1 and
  v2), TLS and fingerprint rules, `entry_aad` layout, mDNS service type and
  TXT keys, delta cursors.
- CI runs on `ubuntu-latest` and `macos-latest`: workspace tests, all-feature
  core tests, the two-device tests, the FFI build on both, plus the universal
  static library and `swift test` on macOS. The 75% region coverage gate keeps
  running on Ubuntu.

### Changed
- FFI: `libresync_last_error` is now per calling thread (errno-style); errors
  from asynchronous tasks arrive as `task_finished` events.
- `SyncRecord` gained `field_clocks` and now implements `Default`; construct
  records with `..SyncRecord::default()`. `LamportClock` implements `Default`.
- `Message::Hello` carries `protocol_version` (defaults to 1 when absent).
- `SyncListener` uses a blocking `accept` woken by a loopback connection on
  shutdown (no 20 ms sleep-polling) and serves each connection on its own
  thread.
