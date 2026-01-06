# Debugging and diagnostics

## CLI
- `libresync diagnose` checks config, keys, state file, selected file, and backups.
- `libresync status` reports paired devices and discovery status.
- `libresync --verbose` prints error chains and debug output.
- `libresync listen --foreground` keeps logs attached to the terminal.
- `libresync discover --timeout-secs 6` helps on slower LANs.

## Common issues
- **No devices discovered**: ensure listeners are running and mDNS is not blocked.
- **Pairing fails**: app IDs must match, and allowlists must permit pairing.
- **Refresh errors**: verify both devices are paired and reachable on the LAN.

## Logs
- `libresync listen` writes to `libresync-listen.log` next to the config file.
- The PID file (`libresync-listen.pid`) is used by `libresync stop`.
