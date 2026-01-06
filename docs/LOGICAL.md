# Logical record sync

Logical adapters map application data into structured records that are merged deterministically. This is the preferred integration path for app state that is naturally record-oriented (notes, tasks, preferences, lists).

## Status
- Logical record sync is the recommended integration path, but it is still early for production apps.
- `SqliteLogicalAdapter` targets a dedicated record table and does not yet map arbitrary existing schemas.
- CLI supports logical record files via `libresync select --kind logical-file`.

## SQLite schema mapping
The SQLite logical adapter can map an existing table into records using a sidecar metadata table:
- Provide a `SqliteLogicalMapping` with a data table, ID column, and column-to-field mapping.
- LibreSync stores clocks and tombstones in a `*_libresync_meta` table by default.
- The mapping is best for a single-table entity with a stable primary key and deterministic columns.
- Use `with_bool_field` or `SqliteLogicalEncoding::Bool` for boolean columns.

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
- `LastWriterWins`: default for most fields (Lamport clock determines the winner).
- `SetUnion`: for list/map fields where you want union semantics.
- `Counter`: prefer per-device maps (`Map` of device IDs to `I64`) so merges take the max per device. If you store a plain `I64`, the newer value wins (no double counting).
- `ListAppend`: appends list entries in order, skipping duplicates to remain idempotent.
- `Custom(name)`: reserved for app-defined merges in future adapter implementations.

Tombstones win over active data when their Lamport clock is newer. If both are tombstones, the newer clock wins.

## Adapter contract
Logical adapters implement `LogicalAdapter`:
- `load_records` loads app data into a `RecordState` before refresh.
- `apply_records` writes the merged snapshot back to the app store after refresh.
- `apply_snapshot` can override merge behavior to apply field-level policies.

## Provided adapters
- `InMemoryLogicalAdapter`: in-memory adapter for testing and early integration.
- `FileLogicalAdapter`: persists records to a JSON file for simple local-first storage.
- `SqliteLogicalAdapter` (feature `sqlite-logical`): persists records to a SQLite table.

## Usage notes
- Keep record IDs stable across devices.
- Use `updated_at` only as metadata; the Lamport clock determines conflict ordering.
- Use one namespace per app to avoid accidental key collisions.
- For long-lived datasets, prune tombstones with `RecordState::compact` using a `RecordCompactionPolicy` (e.g., max age or max count).
- `RecordCompactionSummary` can be used to report how many tombstones were removed in a maintenance pass.
