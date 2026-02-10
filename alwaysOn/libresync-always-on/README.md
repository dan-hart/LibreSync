# LibreSyncAlwaysOn

## Purpose
LibreSyncAlwaysOn is an always-on desktop device that keeps LibreSync data synced while the primary app is closed.

## Scope (initial)
- LAN-only device (no cloud or remote relay).
- Rust + Tauri app targeting macOS, Windows, and Linux.
- Uses the core Engine and adapter stack from `libresync`.
- Basic UI for device list, last sync, and storage footprint.

## Status
- Engine wiring is active with a status dashboard, manual refresh button, per-app backup toggles, snapshot create/preview/restore controls, retention pruning, and a trust panel (linking + fingerprints + auto-accept/auto-approve toggles).
- System tray menu supports show/hide, manual refresh, and quit.

## Next steps
- Extend daemon/service packaging beyond Linux systemd user service (macOS launchd and Windows scheduler hardening).
- Add explicit linking approval flows (beyond auto-accept).
- Make the app ID configurable so it can sync data for specific apps.
