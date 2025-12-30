mod entry;
mod error;
mod identity;
mod device;
mod adapter;
mod engine;
mod protocol;
mod state;
mod sync;

pub use entry::{Entry, LamportClock};
pub use error::{Error, Result};
pub use identity::Identity;
pub use device::DeviceHandler;
pub use adapter::{AdapterKind, DataAdapter, JsonFileAdapter};
pub use engine::{
    DeviceInfo, Engine, EngineConfig, Event, EventSink, PairingDecision, PairingRequest, SyncResult,
};
pub use protocol::{read_message, write_message, Message};
pub use state::State;
pub use sync::{sync_with_device, SyncListener};
