mod entry;
mod error;
mod identity;
mod device;
mod adapter;
mod engine;
mod discovery;
mod keys;
mod protocol;
mod state;
mod sync;

pub use entry::{Entry, LamportClock};
pub use error::{Error, Result};
pub use identity::Identity;
pub use device::DeviceHandler;
pub use adapter::{AdapterCache, AdapterKind, DataAdapter, JsonFileAdapter, SqliteFileAdapter};
pub use engine::{
    AdapterWatch, DeviceInfo, Engine, EngineConfig, Event, EventSink, PairingDecision,
    PairingRequest, SyncResult,
};
pub use discovery::{browse_mdns, register_mdns, DiscoveredDevice, MdnsAdvertiser};
pub use keys::DeviceKeys;
pub use protocol::{read_message, write_message, Message};
pub use state::State;
pub use sync::{pair_with_device, sync_with_device, SyncListener};
