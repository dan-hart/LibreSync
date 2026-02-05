# LibreSyncAlwaysOn

## Purpose
- Always-on desktop app that keeps LibreSync data updated while primary apps are closed.
- Runs as a device on the LAN, not a central host.
- Stores only encrypted data locally (engine-owned E2EE).

## Initial scope
- Rust + Tauri app targeting macOS, Windows, and Linux.
- Reuses the core Engine and adapter stack.
- Device list, last-sync status, and storage footprint UI.
- LAN discovery + linking with explicit consent.
- Backups are opt-in per app; restores require an explicit confirmation setting.
- Manual refresh control and status dashboard in the UI.
- Snapshot list with create/preview/restore controls (restore gated by allow-restore).
- Snapshot retention defaults (count + age) with manual prune.
- System tray menu for show/hide, manual refresh, and quit.
- Trust panel for linking, auto-accept toggle, and fingerprint visibility.

## Code layout
- `alwaysOn/libresync-always-on` contains the current app scaffold.

## Non-goals (initial)
- No cloud relay or hosted sync.
- No remote access outside the LAN.

## Open questions
- Default adapter set to ship with the app.
- Storage limits and retention policy for encrypted state.
- Background service vs. foreground tray behavior on each OS.
- Always-on trust UX (linking prompts, fingerprint visibility, and re-link flows).

## Daemon/service mode
The `libresync-alwayson-daemon` binary provides headless, OS-level background behavior for LibreSyncAlwaysOn. It uses the same config/state layout as the UI and keeps a JSON adapter synced on a timer.

Auto-approve linking is opt-in and intended only for trusted private networks.

### Build and run
From the repo root:
```
cargo run -p libresync-alwayson-daemon -- --auto-accept
```

Common options:
- `--config <path>`: override config path.
- `--data <path>`: JSON file to sync.
- `--state <path>`: encrypted state file path.
- `--listen <addr>`: override listen address.
- `--auto-accept`: automatically accept linking requests.
- `--auto-approve`: automatically discover and link devices on private LANs (implies auto-accept).

### Install as a service
Templates live in `alwaysOn/service/`:
- `libresync-alwayson-daemon.service` (systemd user service)
- `com.codedbydan.libresync.alwayson.plist` (launchd)
- `libresync-alwayson-daemon.xml` (Windows Task Scheduler)

Edit the ExecStart/Command paths to the installed daemon binary and adjust arguments as needed.

Linux systemd user setup:
```
mkdir -p ~/.config/systemd/user
cp alwaysOn/service/libresync-alwayson-daemon.service ~/.config/systemd/user/
# Edit ExecStart to the installed daemon path and add any flags.
systemctl --user daemon-reload
systemctl --user enable --now libresync-alwayson-daemon.service
journalctl --user -u libresync-alwayson-daemon.service -f
```

Security note: only use `--auto-approve` on trusted private LANs, and prefer a `--pairing-secret` when enabling it.
