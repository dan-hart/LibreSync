# Quickstart

LibreSync lets apps sync data directly between devices on the same LAN with mandatory E2EE.

## 1) Install
Homebrew:
```
brew install dan-hart/tap/libresync
```
crates.io:
```
cargo install libresync-cli
```
From a release tag:
```
cargo install --git https://github.com/dan-hart/LibreSync --tag v0.6.1 libresync-cli
```
Or from a local checkout:
```
cargo install --path crates/libresync-cli --force
```

## 2) Build from source (optional)
If you are developing from the repo:
```
cargo build
```

## 3) Initialize configs
On each device:
```
libresync init
```
Output hint: prints the config path, device ID, and fingerprint.

## 4) Pick an adapter to sync
Logical record sync is the preferred integration path:
```
libresync select --id records --kind logical-file --file ~/libresync-records.json
```
Optional: add JSON file adapters:
```
libresync select --id settings --file ~/libresync-settings.json
```
Optional: add a SQLite file adapter:
```
libresync select --id db --kind sqlite --file ~/libresync.db
```
Optional: enable page-delta for SQLite:
```
libresync select --id db --kind sqlite --page-delta 4096 --file ~/libresync.db
```

## 5) Start listeners
```
libresync listen
```

## 6) Link devices (trust only)
```
libresync link
```

## 7) Refresh
```
libresync refresh
```
Refresh all selected adapters:
```
libresync refresh --all-adapters
```

## 8) Watch for changes
```
libresync watch
```

## 9) Diagnostics
```
libresync diagnose
```

## 10) AlwaysOn (optional)
For background sync on Linux, you can run the headless daemon:
```
cargo run -p libresync-alwayson-daemon -- --auto-accept --pairing-secret "my-shared-secret"
```
See `alwaysOn/README.md` for systemd user service setup.

## Notes
- Linking is required before refresh.
- E2EE is always on (no opt-out).
- Use `libresync status` for last-seen and backup status.
 - Backups are opt-in: `libresync backup configure --enable`.
