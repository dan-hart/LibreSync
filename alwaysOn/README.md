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
