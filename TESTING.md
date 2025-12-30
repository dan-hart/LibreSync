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

## 1) Initialize the config on each device
On **Device A**:
```
libresync init --config ~/libresync.json
```

On **Device B**:
```
libresync init --config ~/libresync.json
```

Optional: provide explicit IDs for consistent testing:
```
libresync init --config ~/libresync.json --device-id amber-river-summit --user-id calm-forest
```

## 2) Select the JSON file to sync
On both devices:
```
libresync select --config ~/libresync.json --file ~/libresync-data.json
```

If the file does not exist, LibreSync creates it with `{}`.

## 3) Start listeners on both devices
On **Device A**:
```
libresync listen --config ~/libresync.json
```

On **Device B**:
```
libresync listen --config ~/libresync.json
```

By default, listeners bind to port 52345 and advertise via mDNS.

## 4) Discover devices (optional)
You can verify discovery before pairing:
```
libresync discover --config ~/libresync.json
```

This should list the other device on the LAN with its device ID and address.

## 5) Pair the devices (trust only)
Pairing must happen once between devices, with consent on both sides.

On **Device A**:
```
libresync pair --config ~/libresync.json
```

If multiple devices are discovered, you’ll be prompted to select one.

On **Device B** (optional, if you want to initiate pairing from the other side):
```
libresync pair --config ~/libresync.json
```

Pairing establishes trust only; it does **not** sync data.

## 6) Sync the JSON file
On either device:
```
libresync sync --config ~/libresync.json
```

You’ll be prompted to select a device if more than one is discovered. Sync exchanges the JSON file and applies last‑writer‑wins logic.

## 7) Verify sync
Edit the JSON file on **Device A**:
```
echo '{"from":"device-a"}' > ~/libresync-data.json
```

Run sync from **Device A**:
```
libresync sync --config ~/libresync.json
```

Check **Device B**:
```
cat ~/libresync-data.json
```

Then reverse the direction and repeat to confirm two‑way sync.

## 8) Check status
Use `status` to verify paired devices and last seen info:
```
libresync status --config ~/libresync.json
```

## Troubleshooting
- **No devices found**: ensure both listeners are running and mDNS is not blocked.
- **Pair rejected**: check that app IDs match and both devices accept pairing.
- **Sync fails**: confirm pairing is complete and both devices are still on the same LAN.

## Notes
- Discovery is unauthenticated and only used to find devices.
- Pairing is required before sync.
- The current MVP uses identity strings only; crypto/auth will be added later.
