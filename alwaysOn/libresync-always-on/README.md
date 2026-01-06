# LibreSyncAlwaysOn

## Purpose
LibreSyncAlwaysOn is an always-on desktop device that keeps LibreSync data synced while the primary app is closed.

## Scope (initial)
- LAN-only device (no cloud or remote relay).
- Rust + Tauri app targeting macOS, Windows, and Linux.
- Uses the core Engine and adapter stack from `libresync`.
- Basic UI for device list, last sync, and storage footprint.

## Status
- Engine wiring is active with a status dashboard, manual refresh button, per-app backup toggles, and snapshot preview/restore controls.

## Next steps
- Add a tray + background service behavior per OS.
- Add device list, pairing approvals, and trust management UI.
- Make the app ID configurable so it can sync data for specific apps.
