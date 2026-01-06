# Testing LibreSync between two devices

This guide walks through manual end-to-end testing using two machines on the same LAN, each running the `libresync` CLI.

## Prerequisites
- Both devices are on the same local network (same Wi‑Fi/LAN).
- `libresync` is installed or built on both devices.
- mDNS is available on the LAN (default for most home networks).

## Build the CLI
On each device:
```
cargo build
```

## Automated checks
For local coverage validation:
```
cargo test
cargo llvm-cov --workspace --summary-only --fail-under-regions 75
```

## 1) Initialize the config on each device
On **Device A**:
```
libresync init
```

On **Device B**:
```
libresync init
```

Optional: provide explicit IDs for consistent testing:
```
libresync init --device-id amber-river-summit --user-id calm-forest
```

## 2) Select the JSON file to refresh
On both devices:
```
libresync select --file ~/libresync-data.json
```

If the file does not exist, LibreSync creates it with `{}`.

Optional: add more than one file (each with its own adapter ID):
```
libresync select --id settings --file ~/libresync-settings.json
```

## 3) Start listeners on both devices
On **Device A**:
```
libresync listen
```

On **Device B**:
```
libresync listen
```

By default, listeners bind to port 52345 and advertise via mDNS.
The listener runs in the background by default; use `--foreground` if you want it attached to your terminal.
To stop a background listener, run:
```
libresync stop
```

## 4) Discover devices (optional)
You can verify discovery before linking:
```
libresync discover
```

This should list the other device on the LAN with its device ID and address.

## 5) Link the devices (trust only)
Linking must happen once between devices, with consent on both sides.

On **Device A**:
```
libresync link
```

If multiple devices are discovered, you’ll be prompted to select one.

On **Device B** (optional, if you want to initiate linking from the other side):
```
libresync link
```

Linking establishes trust only; it does **not** refresh data.

## 6) Refresh the JSON file
On either device:
```
libresync refresh
```

To refresh all selected adapters:
```
libresync refresh --all-adapters
```

You’ll be prompted to select a device if more than one is discovered. Refresh exchanges the JSON file and applies last‑writer‑wins logic.

## 7) (Optional) Run watch for real-time updates
On each device:
```
libresync watch
```
By default, `watch` auto-starts a local listener on `0.0.0.0:52345`. Use `--no-listen` if you already run `libresync listen`, or `--listen` to override the bind address.

## 8) Verify refresh
Edit the JSON file on **Device A**:
```
echo '{"from":"device-a"}' > ~/libresync-data.json
```

Run refresh from **Device A**:
```
libresync refresh
```

Check **Device B**:
```
cat ~/libresync-data.json
```

Then reverse the direction and repeat to confirm two‑way refresh.

## 9) Check status
Use `status` to verify linked devices and last seen info:
```
libresync status
```

## Troubleshooting
- **No devices found**: ensure both listeners are running and mDNS is not blocked.
- **Link rejected**: check that app IDs match and both devices accept linking.
- **Sync fails**: confirm linking is complete and both devices are still on the same LAN.

## Notes
- Discovery is unauthenticated and only used to find devices.
- Linking is required before refresh.
- E2EE is enforced by the engine; state files and backups are encrypted at rest.
- Config defaults to the OS config directory; pass `--config` to override.
