# Logical record sync

Logical adapters map application data into structured records that are merged deterministically. This is the preferred integration path for app state that is naturally record-oriented (notes, tasks, preferences, lists).

## Status
- Logical record sync is the recommended integration path, but it is still early for production apps.
- `SqliteLogicalAdapter` can map existing SQLite tables and now supports multi-table mappings in one adapter.
- Complex relational schemas may still require careful field-policy tuning.
- CLI supports logical record files via `libresync select --kind logical-file`.

## SQLite schema mapping
The SQLite logical adapter can map an existing table into records using a sidecar metadata table:
- Provide a `SqliteLogicalMapping` with a data table, ID column, and column-to-field mapping.
- You can provide one or more mappings; each mapping syncs one table/entity pair.
- LibreSync stores clocks and tombstones in a `*_libresync_meta` table by default.
- The mapping is best for a single-table entity with a stable primary key and deterministic columns.
- Use `with_bool_field` or `SqliteLogicalEncoding::Bool` for boolean columns.
- Use `SqliteLogicalEncoding::JsonValue` for JSON columns storing arrays/objects or primitive values.
- Merge policies can be set per field (or as a default on the mapping) to match your schema’s semantics.

## Record shape
Each record is a JSON object with a stable identity and a Lamport clock:

```json
{
  "schema": "notes",
  "entity": "Note",
  "id": "note-123",
  "fields": {
    "title": { "type": "String", "value": "Hello" },
    "tags": { "type": "List", "value": [
      { "type": "String", "value": "work" }
    ] }
  },
  "tombstone": false,
  "clock": { "counter": 42, "device_id": "amber-river-summit" }
}
```

Field values are tagged unions so record data stays lossless across transports:
- `Null`, `Bool`, `I64`, `F64`, `String`, `Bytes`
- `List` of field values
- `Map` of string keys to field values

## Merge policy
Merges happen at the field level and are deterministic across devices:
- `LastWriterWins`: default for most fields (the field's Lamport clock determines the winner).
- `SetUnion`: for list/map fields where you want union semantics.
- `Counter`: prefer per-device maps (`Map` of device IDs to `I64`) so merges take the max per device. If you store a plain `I64`, the newer value wins (no double counting).
- `ListAppend`: appends list entries in order, skipping duplicates to remain idempotent.
- `AppendOnly`: grow-only list of opaque operations (an op-log). Items are unioned and never dropped, even by an older or shorter incoming version. See "Event logs" below.
- `Custom(name)`: uses adapter-provided custom merge hooks. If no hook is registered for `name`, it falls back to last-writer-wins.

Tombstones win over active data when their Lamport clock is newer. If both are tombstones, the newer clock wins.

Merge policies run on every path that brings in remote data: sync (the engine routes inbound entries to the adapter that owns them), backup restore, and `LogicalAdapter::apply_snapshot`.

### Field clocks
A record has one Lamport clock, but apps usually hand the engine whole records. To tell which fields an edit actually touched, the engine keeps a clock per field (`SyncRecord::field_clocks`). You normally leave it empty: when a record is written (`RecordState::set`, `apply`, or adapter `load_records`), fields whose value differs from the stored version get the record's clock and unchanged fields keep their previous clock.

Field-level last-writer-wins compares field clocks, so two devices that edit different fields of the same record concurrently both keep their edit (see `tests/two_device.rs`). Only when both devices change the same field does the newer record clock win.

Field clocks are stored in the sync state and on the wire; adapters that persist records elsewhere (SQLite mappings, JSON files) may drop them, they are re-derived on the next load.

## Adapter contract
Logical adapters implement `LogicalAdapter`:
- `load_records` loads app data into a `RecordState` before refresh.
- `apply_records` writes the merged snapshot back to the app store after refresh.
- `apply_snapshot` can override merge behavior to apply field-level policies.
- `merge_custom_field` can resolve `Custom(name)` policies with app-defined logic.

## Provided adapters
- `InMemoryLogicalAdapter`: in-memory adapter for testing and early integration.
- `FileLogicalAdapter`: persists records to a JSON file for simple local-first storage.
- `SqliteLogicalAdapter` (feature `sqlite-logical`): persists records to a SQLite table or mapped existing tables (single or multi-table mappings).

## Event logs
Apps that sync an operation log (immutable events with their own causality) need a merge that unions entries rather than picking a winner. Two shapes work:

1. **One record per event.** Each op is its own `SyncRecord` with a unique `id` (`ulid`, `uuid`, or `<device>-<seq>`). Distinct ids never conflict, so plain `LastWriterWins` is loss-free. Prefer this for large logs: each event is one small entry and delta sync ships only new events.
2. **One record holding the log** in a field with `MergePolicy::AppendOnly`. The field is a `List` of opaque ops (each a `Map`, `String`, or `Bytes`). Merging unions the lists; nothing is ever removed, and a non-list incoming value is treated as a single op.

Example of shape 2 with `InMemoryLogicalAdapter`:

```rust
use std::collections::BTreeMap;
use libresync::{FieldValue, InMemoryLogicalAdapter, LamportClock, MergePolicy, SyncRecord};

let adapter = InMemoryLogicalAdapter::new("events", "com.example.notes")
    .with_merge_policy("ops", MergePolicy::AppendOnly);

fn op(id: &str, kind: &str) -> FieldValue {
    FieldValue::Map(BTreeMap::from([
        ("id".to_string(), FieldValue::String(id.to_string())),
        ("kind".to_string(), FieldValue::String(kind.to_string())),
    ]))
}

// Device A appends locally (bump the record clock on every write).
adapter.upsert_record(SyncRecord {
    schema: "notes".to_string(),
    entity: "Log".to_string(),
    id: "note-1".to_string(),
    fields: BTreeMap::from([(
        "ops".to_string(),
        FieldValue::List(vec![op("01J...A1", "create"), op("01J...A2", "rename")]),
    )]),
    clock: LamportClock { counter: 2, device_id: "device-a".to_string() },
    ..SyncRecord::default()
});
// Device B concurrently appended op("01J...B1", "tag") with clock (2, "device-b").
// After `engine.sync_now(...)` both devices hold all three ops.
```

Ordering within the merged list is "existing items, then unseen incoming items", so it may differ between devices. Order by the causality metadata inside each op (a ULID, a per-device sequence, or a hybrid logical clock), never by list position. `ListAppend` has the same union semantics for list values but falls back to last-writer-wins for non-list values; `AppendOnly` never falls back.

The test `append_only_event_log_loses_nothing_under_concurrent_appends` in `crates/libresync/tests/two_device.rs` syncs an event log between two engines with interleaved offline appends and checks that no op is lost or duplicated.

## Usage notes
- Keep record IDs stable across devices.
- Use `updated_at` only as metadata; the Lamport clock determines conflict ordering.
- Bump `clock.counter` on every local write. A record written with an unchanged clock is ignored as "already seen".
- Construct records with `..SyncRecord::default()` so new optional fields (like `field_clocks`) do not break your code.
- Use one namespace per app to avoid accidental key collisions.
- For long-lived datasets, prune tombstones with `RecordState::compact` using a `RecordCompactionPolicy` (e.g., max age or max count).
- `RecordCompactionSummary` can be used to report how many tombstones were removed in a maintenance pass.
- `SqliteLogicalEncoding::JsonValue` serializes fields as standard JSON (not the tagged `FieldValue` JSON used by `Json`).
