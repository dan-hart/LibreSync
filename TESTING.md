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

## 2) Select the JSON file to sync
On both devices:
```
libresync select --file ~/libresync-data.json
```

If the file does not exist, LibreSync creates it with `{}`.

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

## 4) Discover devices (optional)
You can verify discovery before pairing:
```
libresync discover
```

This should list the other device on the LAN with its device ID and address.

## 5) Pair the devices (trust only)
Pairing must happen once between devices, with consent on both sides.

On **Device A**:
```
libresync pair
```

If multiple devices are discovered, you’ll be prompted to select one.

On **Device B** (optional, if you want to initiate pairing from the other side):
```
libresync pair
```

Pairing establishes trust only; it does **not** sync data.

## 6) Sync the JSON file
On either device:
```
libresync sync
```

You’ll be prompted to select a device if more than one is discovered. Sync exchanges the JSON file and applies last‑writer‑wins logic.

## 7) Verify sync
Edit the JSON file on **Device A**:
```
echo '{"from":"device-a"}' > ~/libresync-data.json
```

Run sync from **Device A**:
```
libresync sync
```

Check **Device B**:
```
cat ~/libresync-data.json
```

Then reverse the direction and repeat to confirm two‑way sync.

## 8) Check status
Use `status` to verify paired devices and last seen info:
```
libresync status
```

## Troubleshooting
- **No devices found**: ensure both listeners are running and mDNS is not blocked.
- **Pair rejected**: check that app IDs match and both devices accept pairing.
- **Sync fails**: confirm pairing is complete and both devices are still on the same LAN.

## Notes
- Discovery is unauthenticated and only used to find devices.
- Pairing is required before sync.
- The current MVP uses identity strings only; crypto/auth will be added later.
- Config defaults to the OS config directory; pass `--config` to override.
