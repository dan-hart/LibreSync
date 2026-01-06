# CLI usage

The CLI is a thin wrapper around the core library. It covers the full flow to configure adapters (JSON or SQLite), discover devices, link, refresh, manage backups, and rotate or export keys.

## Command summary
- `init`: create a config with device/app/user identity.
- `select`: choose a file-backed adapter to refresh (`--kind json|sqlite|logical-file`).
- `discover`: list devices on the LAN for the same app ID.
- `link`: request linking with a device (mutual consent).
- `device set-address`: store a manual address for a linked device.
- `unlink`: revoke linking with a device (removes from allowlist).
- `listen`: run the device listener and advertise on LAN.
- `refresh`: refresh a selected adapter with a linked device (use `--all` to refresh all linked devices, `--all-adapters` to refresh all selected adapters).
- `watch`: watch selected adapter files and refresh all linked devices (auto-starts a listener by default).
- `stop`: stop the background listener for this config.
- `status`: show identity, selected file, backup settings, listener status, linked devices, and discovered devices (with last seen time).
- `diagnose`: run config and storage diagnostics.
- `key rotate`: rotate the app-level key and re-encrypt state/backups.
- `key rotate-device`: rotate device TLS keys and fingerprint (remote devices must re-link).
- `key export-app`: export the app-level key to a file.
- `key import-app`: import an app-level key and re-encrypt local state/backups.
- `key export-device`: export device keys to a file.
- `key import-device`: import device keys from a file.
- `backup configure`: opt in to encrypted backups (and optionally allow restores).
- `backup snapshot`: create an encrypted snapshot.
- `backup list`: list encrypted snapshots.
- `backup preview`: show a diff summary between a snapshot and current state.
- `backup restore`: restore an encrypted snapshot (requires allow-restore + `--confirm` + `--confirm-id`).
- `backup prune`: delete old snapshots using retention rules.

## Output hints
- `init` prints the config path plus device/user/app IDs and fingerprint.
- `select` prints adapter ID, kind, and path.
- `discover` prints device IDs, user IDs, and addresses.
- `link`/`refresh` print local and remote fingerprints plus per-device summaries.
- `device set-address` prints the stored address and updates last-seen metadata.
- `backup snapshot` prints the snapshot ID and storage path.
- `backup preview`/`restore` print diff summaries and restore status.

## Config file
The config is a JSON file that stores identity and trust state:
- `device_id`: three words separated by dashes.
- `user_id`: adjective-noun link separated by a dash.
- `app_id`: bundle identifier.
- `state_path`: encrypted internal state file for refresh metadata.
- `data_path`: legacy single JSON file (still populated for the default JSON adapter).
- `data_paths`: legacy map of adapter IDs to JSON file paths.
- `adapters`: map of adapter IDs to adapter config (`kind`, `path`, optional `page_delta`).
- `adapters[].namespace`: namespace override for logical-file adapters (defaults to `app_id`).
- `default_adapter`: adapter ID to use when `--adapter-id` is not provided.
- `device_keys`: TLS key material and fingerprint (stored locally).
- `app_key`: app-level encryption key (stored locally).
- `backup_enabled`: whether encrypted backups are enabled.
- `backup_allow_restore`: whether restores are allowed for this app.
- `backup_dir`: optional backup directory override.
- `devices`: map of linked devices keyed by device ID (includes last seen address and fingerprint).

### Default location
If `--config` is omitted, LibreSync uses the OS config directory:
- macOS: `~/Library/Application Support/libresync-cli/libresync.json`
- Linux: `~/.config/libresync-cli/libresync.json`
- Windows: `%APPDATA%\\com\\codedbydan\\libresync-cli\\libresync.json`

You can override the location with `--config` on any command.

## Typical workflow
1. On each device, run `init` to create a config.
2. Run `select` to choose the adapter to refresh (use `--id` to add more than one).
3. Start `listen` on both devices (or at least one). The default listener port is `52345`. The listener runs in the background by default; use `--foreground` to keep it in your terminal.
4. Run `discover` to list devices on the LAN (optional).
5. Run `link` once between devices (consent required). You can either:
   - pass a device address with `--device`, or
   - pass a device ID with `--device-id`, or
   - omit both and select from the discovery list.
6. Use `unlink --device-id <device-id>` to revoke trust and require re-linking.
7. Run `refresh` whenever you want to refresh. The same address/device-ID selection rules apply. Use `refresh --all` to refresh every linked device, or `--all-adapters` to refresh all selected adapters.
8. Run `watch` to refresh all linked devices on local changes and on a periodic interval (supports multiple adapters).
9. Run `status` to see connected devices and last seen addresses.
10. Run `diagnose` to verify config, keys, and storage before linking.

Example: add a SQLite adapter with page-delta encoding:
```
libresync select --id db --kind sqlite --page-delta 4096 --file ./app.db
```
Example: add a logical record adapter:
```
libresync select --id records --kind logical-file --file ./records.json
```

## Notes
- Link establishes trust only; it does not refresh data.
- Refresh exchanges data and requires prior linking.
- Discovery only lists devices; it does not grant trust.
- `link` prints the local and remote fingerprints so you can verify trust out of band.
- `listen` can be run with `--no-discovery` to avoid mDNS advertising.
- `listen` writes logs to `libresync-listen.log` and a PID to `libresync-listen.pid` next to the config file.
- `listen` updates selected adapter files when incoming refreshes are received.
- `stop` reads the PID file next to the config and terminates the listener process.
- `link` and `refresh` will prompt for a device if multiple are discovered and no device is specified.
- `device set-address` lets you store a manual address to use when discovery fails.
- `refresh --all` refreshes all linked devices; add `--no-discover` to use only stored addresses.
- `watch` refreshes all linked devices and uses discovery unless `--no-discover` is set.
- `watch` will start a local listener unless `--no-listen` is set.
- `status` includes backup settings and local storage sizes for the state and selected file.
- `backup restore` requires two steps: `backup configure --allow-restore` plus `backup restore --confirm --confirm-id <snapshot-id>`.
- `backup prune` requires at least one retention rule (`--keep` and/or `--max-age-days`).
- `key rotate` clears the allowlist by default; re-link devices after rotation.
- `key rotate-device` changes the local fingerprint; remote devices must re-link.
- `key import-app` re-encrypts local state/backups with the imported key; use `--confirm`.
- `key import-device` replaces TLS keys and requires a listener restart.
- Use `--verbose` to include debug details when errors occur.
- `logical-file` adapters store an array of `SyncRecord` entries; use `--namespace` to override the default (app ID).
