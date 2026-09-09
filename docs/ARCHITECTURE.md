# Architecture overview

LibreSync is a device-to-device (D2D) sync engine. Every instance is a device, and devices connect directly on the LAN. There is no fixed role in the protocol; a device may accept inbound connections and also initiate outbound connections at any time.

## Core principles
- Devices are symmetric; any device can initiate a connection.
- Trust is scoped per app ID, not per device globally.
- Sync is deterministic: last-writer-wins with Lamport clocks per entry, and per field for logical records.
- The protocol is intentionally minimal and binary-agnostic (JSON lines in the MVP).
- Transport is encrypted (mutual TLS with self-signed device keys, trust on first use, fingerprints pinned after linking).

## SDK core (engine + adapters)
The core library now exposes an SDK-first surface:
- `Engine` owns the listener, sync flow, and shared state.
- `DataAdapter` bridges app data to the sync state (logical and file adapters).
- The CLI is a consumer of the same Engine APIs for listen/refresh/watch.
- `EventSink` and `EventStream` provide sync status callbacks for UIs.

## Identity model
Each device announces a triple:
- device ID: stable identifier (CLI uses three words; apps can provide their own).
- app ID: bundle identifier (e.g., `com.codedbydan.libresync-cli`).
- user ID: human-friendly string (CLI uses adjective + noun).

The app ID is the scope boundary for trust and discovery. Devices will only link and sync when app IDs match.

## Discovery (LAN + private overlay)
- Devices advertise over mDNS with a service type of `_libresync._tcp.local.`
- Advertisements include app ID, device ID, and user ID as TXT properties.
- Discovery also checks private overlays:
  - Tailscale/Headscale peers via `tailscale status --json` when the client is installed.
  - Static overlay peers via `LIBRESYNC_OVERLAY_PEERS` (`ip[:port],ip[:port],...`).
- Discovery is used to find candidate devices, not to establish trust.
 - The default listener port is `52345` unless overridden.
- Stored last-seen addresses (manual or observed) can be used when discovery is unavailable.

## Platform constraints
- iOS/macOS require Local Network permission for discovery and inbound connections.
- Some Wi-Fi networks use AP isolation, which blocks device-to-device traffic even on LAN.
- Manual addresses and fallback refresh flows are required in restricted networks.

## Linking and consent
Linking is explicit consent on both devices:
1. Device A sends a linking request to device B.
2. Device B checks the app ID and prompts the user (or auto-accepts).
3. If accepted, device B stores device A in its allowlist.
4. Device A then prompts locally and stores device B in its allowlist.

Linking is required before any sync. Devices that are not linked are rejected.
Auto-approve linking (when enabled) will automatically discover and link devices on private or link-local networks; it is disabled by default.

## Trust scope (per app)
Trust is stored per app ID. This prevents a trusted device in one app from automatically being trusted by another app.

## Connection flow
A device runs a listener to accept inbound connections. A sync is one connection (protocol version 2):
1. Mutual-TLS handshake with self-signed device certificates; the client pins the peer's fingerprint when it knows it.
2. `Hello` in both directions (identity + protocol version); each side checks app id, linking and the pinned fingerprint.
3. The client sends `SnapshotSince { clock, epoch }`, its cursor into the listener's history.
4. The listener answers `Delta` with the entries applied after that cursor (or a full snapshot on first contact or after a reset), its new cursor, and how far it has received the client's entries.
5. The client merges, then sends its own `Delta` sized by that acknowledgement; the listener merges and answers `Ack`.

Version 1 peers (pre-0.4) still work: the client falls back to the two-connection push/pull exchange, and the listener still answers `SnapshotRequest`/`Snapshot`. See `docs/PROTOCOL.md` for the wire format.

Inbound entries are routed to the adapter that owns their key, so logical adapters merge field by field; entries nobody owns use last-writer-wins.

## Data model
- The core state is a key/value store of bytes.
- Each value has a Lamport clock (counter + device ID).
- On merge, the entry with the higher clock wins. Ties resolve by device ID.

## Logical record sync (primary path)
- Logical adapters map app data into records (`schema`, `entity`, `id`, `fields`, `tombstone`).
- Records are merged with field-level policies (LWW, set-union, counters, list append).
- `InMemoryLogicalAdapter` provides a minimal record adapter for early integrations.
- `FileLogicalAdapter` persists records in a JSON file for simple local storage.
- `SqliteLogicalAdapter` (feature `sqlite-logical`) persists records to a SQLite table and can map existing tables (including multi-table mappings).
- File adapters remain supported as a fallback for arbitrary files or whole-store snapshots.
- See `docs/LOGICAL.md` for the record schema and merge policy details.

## File adapters
- `JsonFileAdapter` and `SqliteFileAdapter` map files into the state.
- `WatchedFileAdapter` uses filesystem events for near-real-time local changes.
- SQLite adapter syncs the main database file plus WAL/SHM sidecars.
- Optional page-delta encoding can be enabled on SQLite adapters to reduce payload size when changes are small.

## Backups
- Backups are opt-in per app and encrypted with the app-level key.
- Snapshots can be previewed before restore, and restore requires explicit confirmation.
- Retention can be enforced via pruning policies (max count and/or age).

## File refresh (CLI)
The CLI maps one or more file-backed adapters (JSON or SQLite) into the state:
- Before refresh, the local file is loaded into the state.
- After refresh, the file is updated with the most recent value.
- Adapter IDs let you refresh multiple files in one session.

## Security notes (MVP)
- Current MVP uses self-signed device keys with TOFU fingerprints for linking.
- Identities are not derived from keys yet (device IDs are user-defined).
- mDNS discovery is unauthenticated and should be treated as a hint only.
- Linking is the trust gate; devices that are not linked are rejected.
- Sync payloads are encrypted by the engine using a shared app-level key (E2EE on the wire, no opt-out).
- Linking exchanges the app-level key inside the TLS channel.
- Engine state files and backups are encrypted at rest with the app-level key.

## LibreSyncAlwaysOn (in progress)
- Always-on desktop device that keeps app data synced while primary apps are closed.
- LAN-only, written in Rust + Tauri, targeting macOS, Windows, and Linux.
- Status dashboard with manual refresh and per-app backup policy controls.
- Snapshot preview and restore controls (restore gated by allow-restore).
- Stored last-seen addresses allow refresh even when discovery is unavailable.
- System tray controls and trust UI (linking, fingerprints, auto-accept toggle).
- Headless daemon mode now provides OS-level background behavior; service templates live under `alwaysOn/service/`.

## Future hardening
- Device keysets and signed app attestations.
- Encrypted transport (Noise or TLS).
- Allowlist revocation and key rotation.
- Trust delegation policies (auto-accept for pre-approved devices).
- Engine-owned E2EE with key rotation and re-keying flows.
