# API stability

LibreSync is pre-1.0, but the following surface is intended to be **stable** and backwards compatible:

## Core runtime
- `Engine`
- `EngineConfig`
- `AutoRefreshConfig`
- `AutoRefresh`
- `Identity`
- `Event`, `EventStream`, `EventSink`

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

## Stability notes
- New fields may be added to public structs over time.
- New enum variants may be added; avoid exhaustive matching in downstream code.
- The CLI is a reference implementation and may change faster than the core library.
- E2EE is enforced by the engine; app key handling is part of the runtime surface.
