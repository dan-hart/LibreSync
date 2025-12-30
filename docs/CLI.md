# CLI usage

The CLI is a thin wrapper around the core library. It exposes the minimal flow needed to discover devices, pair, and refresh a JSON file.

## Command summary
- `init`: create a config with device/app/user identity.
- `select`: choose the JSON file to refresh.
- `discover`: list devices on the LAN for the same app ID.
- `pair`: request pairing with a device (mutual consent).
- `listen`: run the device listener and advertise on LAN.
- `refresh`: refresh the selected JSON file with a paired device.
- `watch`: watch the selected JSON file and refresh all paired devices.
- `stop`: stop the background listener for this config.
- `status`: show identity, selected file, listener status, paired devices, and discovered devices (with last seen time).

## Config file
The config is a JSON file that stores identity and trust state:
- `device_id`: three words separated by dashes.
- `user_id`: adjective-noun pair separated by a dash.
- `app_id`: bundle identifier.
- `state_path`: internal state file for refresh metadata.
- `data_path`: path to the selected JSON file.
- `devices`: map of paired devices keyed by device ID.

### Default location
If `--config` is omitted, LibreSync uses the OS config directory:
- macOS: `~/Library/Application Support/libresync-cli/libresync.json`
- Linux: `~/.config/libresync-cli/libresync.json`
- Windows: `%APPDATA%\\com\\codedbydan\\libresync-cli\\libresync.json`

You can override the location with `--config` on any command.

## Typical workflow
1. On each device, run `init` to create a config.
2. Run `select` to choose the JSON file to refresh.
3. Start `listen` on both devices (or at least one). The default listener port is `52345`. The listener runs in the background by default; use `--foreground` to keep it in your terminal.
4. Run `discover` to list devices on the LAN (optional).
5. Run `pair` once between devices (consent required). You can either:
   - pass a device address with `--device`, or
   - pass a device ID with `--device-id`, or
   - omit both and select from the discovery list.
6. Run `refresh` whenever you want to refresh the file. The same address/device-ID selection rules apply.
7. Run `watch` to refresh all paired devices on local changes and on a periodic interval.
8. Run `status` to see connected devices and last seen addresses.

## Notes
- Pair establishes trust only; it does not refresh data.
- Refresh exchanges data and requires prior pairing.
- Discovery only lists devices; it does not grant trust.
- `listen` can be run with `--no-discovery` to avoid mDNS advertising.
- `listen` writes logs to `libresync-listen.log` and a PID to `libresync-listen.pid` next to the config file.
- `stop` reads the PID file next to the config and terminates the listener process.
- `pair` and `refresh` will prompt for a device if multiple are discovered and no device is specified.
- `watch` refreshes all paired devices and uses discovery unless `--no-discover` is set.
- Use `--verbose` to include debug details when errors occur.
