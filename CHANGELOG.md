# Changelog

All notable changes to LibreSync are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); release notes with
narrative context live in `RELEASES.md`.

## [Unreleased]

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

### Changed
- `SyncRecord` gained `field_clocks` and now implements `Default`; construct
  records with `..SyncRecord::default()`. `LamportClock` implements `Default`.
- `Message::Hello` carries `protocol_version` (defaults to 1 when absent).
- `SyncListener` uses a blocking `accept` woken by a loopback connection on
  shutdown (no 20 ms sleep-polling) and serves each connection on its own
  thread.
