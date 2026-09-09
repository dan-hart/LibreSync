# API stability

LibreSync is pre-1.0, but the following surface is intended to be **stable** and backwards compatible:

## Core runtime
- `Engine`, `SyncRequest`, `SyncStats`
- `BackgroundEngine`, `Ticket`, `CancelToken`
- `EngineConfig`
- `AutoRefreshConfig`
- `AutoRefresh`
- `Identity`
- `Event`, `EventStream`, `EventSink`
- `InboundApplier`, `AdapterRouter`, `LwwApplier`
- `KeyStore` and the provided backends

## Adapters
- `DataAdapter`
- `LogicalAdapter`
- `JsonFileAdapter`
- `SqliteFileAdapter`
- `WatchedFileAdapter`
- `FileLogicalAdapter`
- `SqliteLogicalAdapter` (feature `sqlite-logical`)
- `SqliteLogicalMapping` / `SqliteLogicalField` / `SqliteLogicalEncoding` (feature `sqlite-logical`)

## Logical record types
- `SyncRecord`
- `FieldValue`
- `MergePolicy`
- `RecordState` / `RecordView`
- `RecordCompactionPolicy` / `RecordCompactionSummary`

## Backups
- `BackupManager`
- `FileSnapshotStore`
- `RetentionPolicy`
- `PrunePlan` / `PruneSummary`

## Driving the engine from a GUI

Every `Engine` call is blocking and may take seconds (socket timeouts,
discovery). Never call `sync_now`, `request_link`, or `discover_devices` on the
UI thread. Two supported patterns:

### Pattern A: `Engine::spawn` (recommended for Rust apps)

```rust
let mut engine = Engine::new(config, state, handler);
engine.register_logical_adapter(adapter)?;
let background = engine.spawn()?;           // owns a background thread
let events = background.events();           // EventStream, Clone

background.try_start_listening()?;          // returns immediately
let ticket = background.try_sync_now(addr, "records")?;   // non-blocking
// ... later, if the user presses "cancel":
background.cancel(ticket);
// Register more adapters or attach watches on the engine thread:
background.run(|engine| engine.register_adapter(other_adapter))?;
background.shutdown()?;                     // also stops the listener
```

Every command produces the usual events (`SyncStarted`, `SyncFinished`,
`SyncStats`, `LinkFinished`, `DiscoveryFinished`, `ListenerStarted`, ...) plus
`Event::TaskFinished { ticket, result }` when it completes or was cancelled.
Commands run one at a time in submission order.

### Waking the main loop without polling

`EventStream` offers two integration points:

- `raw_fd()` returns a file descriptor that is readable while events are
  queued. Watch it and drain with `try_recv()` until `None`; draining clears
  the readiness. Do not read from the descriptor yourself.
- `set_waker(|| ...)` registers a callback invoked on the emitting thread each
  time an event is queued; use it to schedule a drain on your main loop.

**glib / GTK 4 (thread + channel, gtk-rs):**

```rust
use glib::{ControlFlow, MainContext};
use std::os::fd::AsRawFd;

let events = background.events();
let fd = events.raw_fd().expect("pipe");
// g_unix_fd_add: the callback runs on the main context when fd is readable.
glib::unix_fd_add_local(fd, glib::IOCondition::IN, move |_, _| {
    while let Ok(Some(event)) = events.try_recv() {
        match event {
            Event::FingerprintChanged { device, fingerprint } => {
                // Show a dialog; if the user accepts, re-link the device.
            }
            Event::SyncFinished { .. } | Event::InboundSync { .. } => {
                // Reload the view from the adapter.
            }
            _ => {}
        }
    }
    ControlFlow::Continue
});
```

If you prefer channels to descriptors, use the waker with a glib channel:

```rust
let (tx, rx) = async_channel::unbounded::<()>();
events.set_waker(move || { let _ = tx.try_send(()); });
MainContext::default().spawn_local(async move {
    while rx.recv().await.is_ok() {
        while let Ok(Some(event)) = events_for_loop.try_recv() { /* ... */ }
    }
});
```

Both forms keep GTK's single-threaded model: the engine's own threads never
touch widgets, and the main context only runs the drain.

**Swift / SwiftUI (DispatchQueue):**

The C ABI exposes the same queue (`libresync_engine_event_next`,
`libresync_engine_event_fd`, `libresync_engine_event_wait`) and a non-blocking
sync (`libresync_engine_sync_async` + `libresync_engine_cancel`). The Swift
package wraps them:

```swift
let engine = try LibreSyncEngine(configJson: json, statePath: path)
let pump = engine.makeEventPump(queue: .main) { event in      // DispatchSource on the fd
    switch event.type {
    case "fingerprint_changed": askUserToRelink(event.device)
    case "sync_finished", "inbound_sync": model.reload()
    default: break
    }
}
let ticket = try engine.syncAsync(address: peer, adapterId: "records")
// ... engine.cancel(ticket: ticket)
```

For a dedicated worker instead of a descriptor, run
`engine.waitEvent(timeoutMs:)` in a loop on a background `DispatchQueue` and
`DispatchQueue.main.async` the handler. Blocking calls such as `syncNow` belong
on a background queue, and `LibreSyncEngine` is safe to share across queues.

### Cancellation

`CancelToken` (attached through `SyncRequest::with_cancel` or
`SyncOptions::with_cancel`) is checked between protocol steps and shuts down
the socket, so a stalled peer does not hold the caller until the socket
timeout. Cancelled operations return `Error::Cancelled`.

## Migration to 0.4

### `Engine`
- `sync_now` keeps its signature but now exchanges deltas over one connection
  and merges inbound entries through the registered adapters. Use
  `sync_now_with_stats` / `sync(&SyncRequest)` for traffic counters,
  fingerprint pinning (`SyncRequest::for_device`), and cancellation.
- Match on `Event` non-exhaustively: `FingerprintChanged`, `InboundSync`,
  `SyncStats`, `LinkFinished`, `DiscoveryFinished`, `ListenerStarted`,
  `ListenerStopped`, `TaskFinished` were added.
- `Error` gained `FingerprintMismatch { .. }` and `Cancelled`.
- Persist the `State` as before; it now also stores apply sequences, origins
  and peer cursors. Files written by 0.3 load unchanged (the first sync after
  upgrading is a full snapshot).
- The listener merges inbound data through the adapters and writes it back
  (`apply_from_state`), so a `watch` is only needed for local file changes.

### `JsonFileAdapter` (and other `DataAdapter`s)
- No code change. `owns_key` is implemented for the bundled adapters. A custom
  `DataAdapter` that overrides `apply_entries` must also override `owns_key`
  to receive entries during sync; otherwise its entries are merged with
  last-writer-wins as before.

### `LogicalAdapter`
- No trait change. `apply_snapshot` (and therefore `merge_policy` /
  `merge_custom_field`) now runs during sync, not only on restore.
- `SyncRecord` gained `field_clocks` and implements `Default`: replace
  exhaustive struct literals with `SyncRecord { ..., ..SyncRecord::default() }`.
  Leave `field_clocks` empty; the engine derives it. Records handed to
  `load_records` must carry a bumped `clock` on every local edit.
- `MergePolicy::AppendOnly` is new; matches on `MergePolicy` should have a
  wildcard arm.

### Wire protocol
- Version 2 (`SnapshotSince` / `Delta`). 0.4 peers talk to 0.3 peers through
  the legacy push/pull path, and answer 0.3 clients. See `docs/PROTOCOL.md`.

## Stability notes
- New fields may be added to public structs over time.
- New enum variants may be added; avoid exhaustive matching in downstream code.
- `LogicalAdapter::merge_custom_field` is the extension hook for `MergePolicy::Custom(name)`.
- The CLI is a reference implementation and may change faster than the core library.
- E2EE is enforced by the engine; app key handling is part of the runtime surface.
