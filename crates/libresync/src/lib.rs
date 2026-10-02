mod adapter;
mod backup;
pub mod companion;
mod crypto;
mod device;
mod discovery;
mod engine;
mod entry;
mod error;
mod identity;
mod keys;
mod keystore;
mod logical;
mod pairing;
mod protocol;
mod record;
mod session;
#[cfg(feature = "sqlite-logical")]
mod sqlite_logical;
mod state;
mod sync;

pub use adapter::{
    AdapterCache, AdapterCapabilities, AdapterKind, AdapterRouter, DataAdapter, InboundApplier,
    JsonFileAdapter, LogicalAdapter, LogicalAdapterWrapper, LwwApplier, SqliteFileAdapter,
    WatchedFileAdapter,
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
#[cfg(all(unix, feature = "tailscale-local-api"))]
pub use discovery::browse_tailscale_local_api;
pub use discovery::{
    browse_mdns, browse_mdns_metadata, browse_private_overlays, discover_devices, register_mdns,
    register_mdns_metadata, register_mdns_metadata_with_invitation, reset_tailscale_probe,
    tailscale_cli_state, DiscoveredDevice, DiscoveredPeer, DiscoverySource, MdnsAdvertiser,
    PeerAdvertisement, TailscaleCliState, DEFAULT_SYNC_PORT, DEFAULT_TAILSCALE_SOCKET,
    SERVICE_TYPE, TAILSCALE_SOCKET_ENV,
};
pub use engine::{
    event_channel, AdapterWatch, AutoRefresh, AutoRefreshConfig, BackgroundEngine, DeviceInfo,
    Engine, EngineConfig, Event, EventSink, EventStream, LinkingDecision, LinkingRequest,
    SyncRequest, SyncResult, Ticket,
};
pub use entry::{Entry, LamportClock};
pub use error::{Error, Result};
pub use identity::Identity;
pub use keys::DeviceKeys;
pub use keystore::{
    platform_key_store, FileKeyStore, KeyStore, KeyStoreExt, MemoryKeyStore, SecretToolKeyStore,
    SecurityCliKeyStore, APP_KEY_ITEM, DEVICE_CERT_ITEM, DEVICE_KEY_ITEM,
};
pub use logical::{FileLogicalAdapter, InMemoryLogicalAdapter};
pub use pairing::{
    link_secure, recover_secure_link, AdapterDescriptor, AppManifest, DeviceMetadata,
    PairChallenge, PairHello, PairingDescriptor, PairingInvitation, PairingManager,
    SecurePairingOutcome, PAIRING_VERSION,
};
pub use protocol::{read_message, write_message, Message, PROTOCOL_VERSION};
pub use record::{
    entry_to_record, parse_record_key, record_entry_key, record_to_entry, FieldValue, MergePolicy,
    RecordCompactionPolicy, RecordCompactionSummary, RecordKeyParts, RecordState, RecordView,
    SyncRecord,
};
pub use session::{
    AckDisposition, AdapterInspection, ApplicationInbox, BootstrapDecision, BootstrapPreview,
    DiagnosticAction, DiagnosticEvidence, ExportBatch, ManagedAdapter, ManagedReceipt,
    ManagedRecord, PeerSnapshot, PeerState, PermissionState, PreparedChange, RecordsAdapter,
    RecoverySnapshot, Session, SessionConfig, SessionErrorCode, SessionEvent, SessionInvitation,
    SessionPhase, SessionSnapshot,
};
#[cfg(feature = "sqlite-logical")]
pub use sqlite_logical::{
    SqliteLogicalAdapter, SqliteLogicalEncoding, SqliteLogicalField, SqliteLogicalMapping,
};
pub use state::{PeerCursor, State};
pub use sync::{
    link_with_device, link_with_device_using, sync_with_device, sync_with_device_shared,
    sync_with_device_using, CancelToken, ListenerOptions, SyncListener, SyncOptions, SyncOutcome,
    SyncStats,
};

#[cfg(target_os = "macos")]
pub use keystore::NativeKeychainKeyStore;
