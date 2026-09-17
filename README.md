# LibreSync

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/libresync-logo-dark.png">
    <source media="(prefers-color-scheme: light)" srcset="docs/assets/libresync-logo.png">
    <img src="docs/assets/libresync-logo.png" alt="LibreSync logo" width="640">
  </picture>
</p>

<p align="center">
  <strong>Your data. Your devices. Your sync.</strong><br>
  Encrypted, local-first synchronization for applications that do not need a cloud backend.
</p>

<p align="center">
  <a href="https://github.com/dan-hart/LibreSync/releases/latest"><img src="https://img.shields.io/github/v/release/dan-hart/LibreSync?style=for-the-badge&amp;logo=github&amp;logoColor=white&amp;labelColor=24292f&amp;color=FF6600" alt="Latest release"></a>
  <a href="https://github.com/dan-hart/LibreSync/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/dan-hart/LibreSync/ci.yml?branch=main&amp;style=for-the-badge&amp;logo=githubactions&amp;logoColor=white&amp;label=CI&amp;labelColor=24292f" alt="CI status on main"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-AGPL--3.0--only-FF6600?style=for-the-badge&amp;labelColor=24292f" alt="License: AGPL-3.0-only"></a>
</p>
<p align="center">
  <a href="crates/libresync"><img src="https://img.shields.io/badge/built_with-Rust-FF6600?style=flat-square&amp;logo=rust&amp;logoColor=white&amp;labelColor=24292f" alt="Built with Rust"></a>
  <a href="SECURITY.md"><img src="https://img.shields.io/badge/E2EE-always_on-2ea44f?style=flat-square&amp;labelColor=24292f" alt="End-to-end encryption: always on"></a>
  <a href="docs/ARCHITECTURE.md"><img src="https://img.shields.io/badge/sync-device_to_device-FF6600?style=flat-square&amp;labelColor=24292f" alt="Device-to-device sync"></a>
  <a href="PRIVACY.md"><img src="https://img.shields.io/badge/cloud_backend-not_required-2ea44f?style=flat-square&amp;labelColor=24292f" alt="No cloud backend required"></a>
</p>

<p align="center">
  <a href="#quick-start">Quick start</a> ·
  <a href="#build-with-libresync">Rust integration</a> ·
  <a href="#documentation">Documentation</a> ·
  <a href="CHANGELOG.md">Changelog</a>
</p>

LibreSync is an open-source Rust sync engine for sharing application data between trusted devices on a LAN or an optional private overlay. It combines device discovery, explicit linking, encrypted data exchange, and deterministic conflict resolution without a LibreSync account or hosted sync service.

Use it for notes, tasks, preferences, local databases, and other structured app data. The repository includes the Rust library, a CLI, a C ABI with Swift/Kotlin wrappers, and an optional AlwaysOn desktop companion and daemon.

**Current release: [v0.6.1](https://github.com/dan-hart/LibreSync/releases/tag/v0.6.1).** LibreSync is pre-1.0. Desktop is the current focus; mobile SDKs and complex application integrations still need validation. CI exercises Linux and macOS; Windows is a desktop target but is not currently in the CI matrix.

## Why LibreSync?

- **Keep control of your data.** Sync between your own devices without operating a backend or requiring cloud accounts.
- **Work locally, catch up later.** Each device retains its data; peers exchange updates when they can connect again.
- **Encrypt by default.** The engine enforces XChaCha20-Poly1305 payload encryption over TLS and encrypts its persisted state and backups.
- **Trust devices explicitly.** Linking establishes per-app trust and exchanges app keys. Certificate fingerprints are pinned after linking; changed fingerprints require attention and re-linking.
- **Merge at the right level.** Logical records support per-field clocks and configurable merge policies, so concurrent changes to different fields can survive synchronization.
- **Send what changed.** Protocol v2 exchanges deltas over one connection, with a full snapshot on first contact or after a state reset.
- **Keep the UI responsive.** A background engine, event queues, wakers, and cancellation support desktop app integration.

## How it works

1. **Discover** peers with the same app ID through LAN mDNS, configured addresses, or private-overlay discovery.
2. **Link** with consent, verify fingerprints, and establish the shared app key. Discovery alone grants no trust.
3. **Sync** through registered adapters. The engine encrypts outgoing data, merges incoming updates, and writes them to local storage.
4. **Stay current** with file watching or periodic refresh. An optional AlwaysOn peer can remain available while your main app is closed.

LAN sync does not require internet access. Optional overlays such as Tailscale or Headscale can connect devices across networks; their connectivity and relay behavior depend on the overlay configuration.

## Quick start

### Install

With Homebrew on macOS or Linux:

```sh
brew install dan-hart/tap/libresync
```

With a Rust toolchain, install the published Git tag:

```sh
cargo install --git https://github.com/dan-hart/LibreSync --tag v0.6.1 libresync-cli
```

Or install from a checkout:

```sh
git clone https://github.com/dan-hart/LibreSync.git
cd LibreSync
cargo install --path crates/libresync-cli --locked
```

The crates are not yet published to crates.io. Use Homebrew, a Git tag, or a local checkout.

### Sync between two devices

Use two devices on the same LAN for this first run. On **both devices**, initialize the same app ID and select a demo JSON file:

```sh
libresync init --app-id com.example.notes
libresync select --file ./libresync-demo.json
libresync listen --foreground
```

Keep each listener terminal open so you can accept the incoming linking prompt. The default listener port is TCP `52345`; LAN discovery uses mDNS on UDP `5353`.

In a **second terminal on one device**, discover and link:

```sh
libresync discover
libresync link
```

Approve the request in the other device's listener terminal, then confirm locally. Check the reported fingerprints against the other device through a trusted channel. Linking establishes trust; it does not transfer your application data yet.

Edit `libresync-demo.json` on one device, then exchange updates:

```sh
libresync refresh
libresync status
```

For ongoing updates, run this in the second terminal on each device:

```sh
libresync watch --no-listen
```

`--no-listen` reuses the listener you already started. For later sessions, `libresync listen` starts a background listener and `libresync stop` stops it. Alternatively, `libresync watch` starts its own listener when no listener is running. Stop foreground listeners and watchers with Ctrl+C.

### More CLI options

| Task | Command |
| --- | --- |
| Select logical records | `libresync select --id records --kind logical-file --file ./records.json` |
| Select a SQLite file | `libresync select --id db --kind sqlite --file ./app.db` |
| Enable SQLite page deltas | `libresync select --id db --kind sqlite --page-delta 4096 --file ./app.db` |
| Refresh all registered adapters | `libresync refresh --all-adapters` |
| Refresh every linked device | `libresync refresh --all` |
| Link at a known address | `libresync link --device 192.0.2.10:52345` |
| Remove a trusted device | `libresync unlink --device-id <device-id>` |
| Enable encrypted backups | `libresync backup configure --enable` |
| Create a snapshot | `libresync backup snapshot --note "before import"` |
| Diagnose connectivity | `libresync diagnose` |
| Inspect available commands | `libresync --help` |

Use matching adapter IDs and logical namespaces on peers. A logical-file adapter stores LibreSync records; it is not an arbitrary JSON document. For the address example, replace the documentation address with your peer's reachable IP.

Configuration defaults to the OS config directory; commands accept `--config` for another path. Use `--verbose` for error details. Discovery also checks Tailscale/Headscale peers when the `tailscale` CLI is available; `LIBRESYNC_OVERLAY_PEERS` accepts comma-separated `ip[:port]` addresses. See the [CLI reference](docs/CLI.md) and [two-device testing guide](TESTING.md).

## Build with LibreSync

Add the core library from a release tag:

```toml
[dependencies]
libresync = { git = "https://github.com/dan-hart/LibreSync", tag = "v0.6.1" }
```

Enable `features = ["sqlite-logical"]` on that dependency to use SQLite record mapping. The optional `tailscale-local-api` feature supports discovery through Tailscale's local API socket where the CLI is unavailable.

### Choose an adapter

| Adapter | Best for |
| --- | --- |
| `FileLogicalAdapter` | Structured records persisted in a JSON file |
| `SqliteLogicalAdapter` | Record tables or mappings over existing SQLite tables, including multiple tables; requires `sqlite-logical` |
| `InMemoryLogicalAdapter` | Merge-policy experiments and tests |
| `JsonFileAdapter` | Whole-file JSON synchronization using last-writer-wins |
| `SqliteFileAdapter` | SQLite snapshots with WAL/SHM sidecars and optional page deltas |
| `WatchedFileAdapter` | File adapters with local change notifications |

Logical adapters are the recommended path for structured app data. Policies include last-writer-wins, set union, counters, list append, append-only logs, and app-defined custom merges. Keep record IDs stable and advance the record clock for every local edit. See [logical record sync](docs/LOGICAL.md) for schema and merge semantics.

### Integrate the engine

Create an `Engine` with your identity, persisted state, and a `DeviceHandler` that manages consent, trust, and key material. Register adapters, start listening, discover and link peers, then request sync or enable auto-refresh.

For GUI apps, use `Engine::spawn()` to obtain a `BackgroundEngine`. Subscribe to its events and submit sync work without blocking the UI. `EventStream` supports draining, wakers, and a readiness descriptor on supported platforms; cancellation lets the app stop pending sync operations. The [API guide](docs/API.md) includes integration examples and the 0.6 migration notes.

A small [logical-record example](crates/libresync/examples/logical_record_sync.rs) demonstrates record creation and persistence:

```sh
cargo run -p libresync --example logical_record_sync
```

This example writes `./records.json`; it does not establish a two-device sync session.

### Swift, Kotlin, and C

The [`libresync-ffi`](crates/libresync-ffi) crate exposes C ABI version 2, including an event queue, asynchronous sync, and cancellation.

- **Swift:** a Swift package, `LibreSyncEventPump`, key-storage helpers, and SQLite mapping samples. macOS CI builds the universal FFI library and runs Swift package tests.
- **Kotlin:** an early JNI-style wrapper with key-storage helpers and Room-style SQLite mapping samples. Native packaging and app integration still need work.
- **C and other languages:** start with the [C header](bindings/include/libresync.h) and [SDK surface](docs/SDK.md).

See the [bindings overview](bindings/README.md), [macOS packaging](docs/PACKAGING-MACOS.md), and [Linux packaging](docs/PACKAGING-LINUX.md) for build steps and platform requirements.

## AlwaysOn companion

[LibreSyncAlwaysOn](alwaysOn/README.md) is an optional Rust + Tauri desktop app that acts as another sync peer. It provides a status dashboard, tray controls, a trust panel, manual refresh, and opt-in snapshots with preview, restore, and retention controls.

The workspace also includes `libresync-alwayson-daemon` for headless operation. Linux systemd and macOS launchd templates live in [`contrib/`](contrib/README.md); additional service templates are in [`alwaysOn/service/`](alwaysOn/service/). An always-on peer is optional and does not become a central authority.

## Security and current limits

- Payloads, engine state files, and snapshots are encrypted. Application files managed by adapters are not automatically encrypted at rest; apps remain responsible for their local data and key storage.
- The `KeyStore` interface includes file, Linux Secret Service, and macOS Keychain backends. Apps must choose and integrate the appropriate backend.
- Initial linking uses trust on first use. Verify fingerprints through a trusted channel; later mismatches are rejected rather than silently trusted.
- Removing a device revokes its allowlist entry. Guided re-keying remains unfinished; use the documented key-rotation workflow when shared keys must change.
- Devices must be reachable at the same time to exchange updates. LAN firewalls, Wi-Fi client isolation, and platform local-network permissions can prevent discovery or sync.
- Logical records and complex relational schemas still require application-specific merge design and testing. File adapters do not provide record-level merging.
- Mobile SDKs are early. Platform background limits and packaging require additional integration work; desktop CI coverage is not a mobile support guarantee.

Read the [security policy](SECURITY.md), [privacy policy](PRIVACY.md), and [wire protocol](docs/PROTOCOL.md). Report vulnerabilities through [GitHub private vulnerability reporting](https://github.com/dan-hart/LibreSync/security/advisories/new).

## Development

From the repository root:

```sh
cargo build --workspace
cargo test --workspace
cargo test -p libresync --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

CI runs Rust tests and strict Clippy on Linux and macOS, two-device sync tests, FFI builds/tests, and macOS Swift builds/tests. The Linux release checks also run repository and dependency audits, release-version checks, an AlwaysOn smoke build, and a **75% region-coverage minimum**. The CI badge reports workflow status; the coverage threshold is a configured gate, not a live coverage measurement.

Before contributing, follow [CONTRIBUTING.md](CONTRIBUTING.md) for the required git-secrets and ASP hooks, and read the [code of conduct](CODE_OF_CONDUCT.md). See [TESTING.md](TESTING.md) for manual and automated validation and the [release guide](docs/RELEASING.md) for release checks.

## Documentation

| Start here | Go deeper |
| --- | --- |
| [Quickstart](docs/QUICKSTART.md) | [Architecture](docs/ARCHITECTURE.md) |
| [CLI reference](docs/CLI.md) | [Wire protocol](docs/PROTOCOL.md) |
| [Logical record sync](docs/LOGICAL.md) | [API stability and GUI integration](docs/API.md) |
| [Bindings](bindings/README.md) | [SDK surface](docs/SDK.md) |
| [AlwaysOn](alwaysOn/README.md) | [Service and packaging templates](contrib/README.md) |
| [Debugging](docs/DEBUGGING.md) | [Linux packaging](docs/PACKAGING-LINUX.md) / [macOS packaging](docs/PACKAGING-MACOS.md) |
| [Why LibreSync](docs/WHY.md) | [Research](RESEARCH.md) |
| [Changelog](CHANGELOG.md) | [Release history](RELEASES.md) |

## License

LibreSync is licensed under [GNU AGPL v3 only](LICENSE) (`AGPL-3.0-only`).
