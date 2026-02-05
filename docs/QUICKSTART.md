# Quickstart

LibreSync lets apps sync data directly between devices on the same LAN with mandatory E2EE.

## 1) Build
```
cargo build
```

## 2) Initialize configs
On each device:
```
libresync init
```
Output hint: prints the config path, device ID, and fingerprint.

## 3) Pick an adapter to sync
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

## 4) Start listeners
```
libresync listen
```

## 5) Link devices (trust only)
```
libresync link
```

## 6) Refresh
```
libresync refresh
```
Refresh all selected adapters:
```
libresync refresh --all-adapters
```

## 7) Watch for changes
```
libresync watch
```

## 8) Diagnostics
```
libresync diagnose
```

## 9) AlwaysOn (optional)
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
