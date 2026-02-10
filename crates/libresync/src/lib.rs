mod adapter;
mod backup;
mod crypto;
mod device;
mod discovery;
mod engine;
mod entry;
mod error;
mod identity;
mod keys;
mod logical;
mod protocol;
mod record;
#[cfg(feature = "sqlite-logical")]
mod sqlite_logical;
mod state;
mod sync;

pub use adapter::{
    AdapterCache, AdapterCapabilities, AdapterKind, DataAdapter, JsonFileAdapter, LogicalAdapter,
    LogicalAdapterWrapper, SqliteFileAdapter, WatchedFileAdapter,
};
pub use backup::{
    summarize_snapshot_diff, BackupAdapter, BackupManager, DataAdapterBackup, DiffCounts,
    FileSnapshotStore, PrunePlan, PruneSummary, RestoreOptions, RetentionPolicy, Snapshot,
    SnapshotDiffSummary, SnapshotMetadata, SnapshotStore,
};
pub use crypto::{
    decrypt_blob, decrypt_entries, decrypt_entry, encrypt_blob, encrypt_entries, encrypt_entry,
    AppKey,
};
pub use device::DeviceHandler;
pub use discovery::{
    browse_mdns, browse_private_overlays, discover_devices, register_mdns, DiscoveredDevice,
    DiscoverySource, MdnsAdvertiser, DEFAULT_SYNC_PORT,
};
pub use engine::{
    event_channel, AdapterWatch, AutoRefresh, AutoRefreshConfig, DeviceInfo, Engine, EngineConfig,
    Event, EventSink, EventStream, LinkingDecision, LinkingRequest, SyncResult,
};
pub use entry::{Entry, LamportClock};
pub use error::{Error, Result};
pub use identity::Identity;
pub use keys::DeviceKeys;
pub use logical::{FileLogicalAdapter, InMemoryLogicalAdapter};
pub use protocol::{read_message, write_message, Message};
pub use record::{
    entry_to_record, parse_record_key, record_entry_key, record_to_entry, FieldValue, MergePolicy,
    RecordCompactionPolicy, RecordCompactionSummary, RecordKeyParts, RecordState, RecordView,
    SyncRecord,
};
#[cfg(feature = "sqlite-logical")]
pub use sqlite_logical::{
    SqliteLogicalAdapter, SqliteLogicalEncoding, SqliteLogicalField, SqliteLogicalMapping,
};
pub use state::State;
pub use sync::{link_with_device, sync_with_device, SyncListener};
