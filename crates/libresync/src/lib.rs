mod entry;
mod error;
mod identity;
mod device;
mod adapter;
mod engine;
mod discovery;
mod keys;
mod crypto;
mod protocol;
mod record;
mod logical;
#[cfg(feature = "sqlite-logical")]
mod sqlite_logical;
mod backup;
mod state;
mod sync;

pub use entry::{Entry, LamportClock};
pub use error::{Error, Result};
pub use identity::Identity;
pub use device::DeviceHandler;
pub use adapter::{
    AdapterCache, AdapterCapabilities, AdapterKind, DataAdapter, JsonFileAdapter, LogicalAdapter,
    LogicalAdapterWrapper, SqliteFileAdapter, WatchedFileAdapter,
};
pub use engine::{
    AdapterWatch, AutoRefresh, AutoRefreshConfig, DeviceInfo, Engine, EngineConfig, Event,
    EventSink, EventStream, LinkingDecision, LinkingRequest, SyncResult, event_channel,
};
pub use discovery::{browse_mdns, register_mdns, DiscoveredDevice, MdnsAdvertiser};
pub use keys::DeviceKeys;
pub use crypto::{
    AppKey, decrypt_blob, decrypt_entries, decrypt_entry, encrypt_blob, encrypt_entries,
    encrypt_entry,
};
pub use protocol::{read_message, write_message, Message};
pub use record::{
    FieldValue, MergePolicy, RecordCompactionPolicy, RecordCompactionSummary, RecordKeyParts,
    RecordState, RecordView, SyncRecord,
    entry_to_record, parse_record_key, record_entry_key, record_to_entry,
};
pub use logical::{FileLogicalAdapter, InMemoryLogicalAdapter};
#[cfg(feature = "sqlite-logical")]
pub use sqlite_logical::{
    SqliteLogicalAdapter, SqliteLogicalEncoding, SqliteLogicalField, SqliteLogicalMapping,
};
pub use backup::{
    BackupAdapter, BackupManager, DataAdapterBackup, DiffCounts, FileSnapshotStore, PrunePlan,
    PruneSummary, RetentionPolicy, RestoreOptions, Snapshot, SnapshotDiffSummary, SnapshotMetadata,
    SnapshotStore, summarize_snapshot_diff,
};
pub use state::State;
pub use sync::{link_with_device, sync_with_device, SyncListener};
