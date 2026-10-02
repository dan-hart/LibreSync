# LibreSync AlwaysOn desktop app

This Tauri 2 app runs the managed multi-app companion dashboard. See [AlwaysOn setup](../README.md) for supported app contracts, authenticated QR/code pairing, merge consent, secure storage, recovery exports, daemon/service setup and explicit legacy migration.

The frontend is bundled local HTML/CSS/JavaScript; every IPC action uses the real companion manager and runs blocking work off the webview thread. No internet assets or ambient automatic approval are used.
