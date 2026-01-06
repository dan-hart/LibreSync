# LibreSyncAlwaysOn

## Purpose
LibreSyncAlwaysOn is an always-on desktop device that keeps LibreSync data synced while the primary app is closed.

## Scope (initial)
- LAN-only device (no cloud or remote relay).
- Rust + Tauri app targeting macOS, Windows, and Linux.
- Uses the core Engine and adapter stack from `libresync`.
- Basic UI for device list, last sync, and storage footprint.

## Status
- This folder contains the initial scaffolding for the app. The UI is minimal and the engine wiring is a stub.

## Next steps
- Initialize Engine + adapters in `src-tauri/src/main.rs`.
- Add a tray + background service behavior per OS.
- Add device list, pairing, and status UI.
- Make the app ID configurable so it can sync data for specific apps.
