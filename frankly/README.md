# Frankly

Frankly is a small Tauri + Rust demo app that exercises LibreSync at the application level. It provides a single shared todo list backed by SQLite and syncs via LibreSync logical records mapped onto the SQLite table.

## What it does
- Local-first todo list stored in SQLite.
- SQLite logical mapping via a LibreSync metadata table for deterministic merges.
- LAN device linking and sync using LibreSync.
- Automatic listener + mDNS advertising on startup.

## Running locally
From `frankly/src-tauri`:

```bash
cargo tauri dev
```

If you do not have the Tauri CLI installed yet:

```bash
cargo install tauri-cli
```

## Icon requirement
- Tauri expects an RGBA icon at `frankly/src-tauri/icons/icon.png`.
- The repo includes a tiny placeholder so dev builds work; replace it with a real app icon when needed.

## Files and paths
- Config: OS app data dir `config.json` (device identity, keys, linked devices).
- State: OS app data dir `state.json` (LibreSync state).
- Database: OS app data dir `todos.sqlite`.

## App identity
- App ID: `com.codedbydan.Frankly`

## Notes
- Linking auto-accepts in this MVP.
- Tauri global API is enabled for the static HTML UI.
- CSP is disabled in `tauri.conf.json` for simplicity.
