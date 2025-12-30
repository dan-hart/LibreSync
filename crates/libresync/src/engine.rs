use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::{
    DataAdapter, DeviceHandler, Error, Identity, Result, State, SyncListener, sync_with_device,
};

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub identity: Identity,
    pub listen_addr: Option<SocketAddr>,
}

impl EngineConfig {
    pub fn new(identity: Identity) -> Self {
        Self {
            identity,
            listen_addr: None,
        }
    }

    pub fn with_listen_addr(mut self, listen_addr: SocketAddr) -> Self {
        self.listen_addr = Some(listen_addr);
        self
    }
}

#[derive(Clone, Debug)]
pub struct DeviceInfo {
    pub identity: Identity,
    pub address: Option<SocketAddr>,
    pub last_seen: Option<SystemTime>,
    pub paired: bool,
}

#[derive(Clone, Debug)]
pub struct PairingRequest {
    pub device: DeviceInfo,
    pub code: Option<String>,
}

#[derive(Clone, Debug)]
pub enum PairingDecision {
    Accept,
    Reject,
}

#[derive(Clone, Debug)]
pub enum SyncResult {
    Success,
    Failed(String),
}

#[derive(Clone, Debug)]
pub enum Event {
    PairingRequested { request: PairingRequest },
    PairingDecisionRequired { request: PairingRequest },
    SyncStarted { device: DeviceInfo, adapter_id: String },
    SyncFinished {
        device: DeviceInfo,
        adapter_id: String,
        result: SyncResult,
    },
    DeviceSeen { device: DeviceInfo },
    DeviceOffline { device: DeviceInfo },
    Error { message: String },
}

pub trait EventSink: Send + Sync {
    fn emit(&self, event: Event);
}

pub struct Engine {
    config: EngineConfig,
    state: Arc<Mutex<State>>,
    adapters: HashMap<String, Arc<dyn DataAdapter>>,
    event_sink: Option<Arc<dyn EventSink>>,
    listener: Option<SyncListener>,
    listener_addr: Option<SocketAddr>,
    device_handler: Arc<dyn DeviceHandler>,
}

impl Engine {
    pub fn new(
        config: EngineConfig,
        state: State,
        device_handler: Arc<dyn DeviceHandler>,
    ) -> Self {
        Self {
            config,
            state: Arc::new(Mutex::new(state)),
            adapters: HashMap::new(),
            event_sink: None,
            listener: None,
            listener_addr: None,
            device_handler,
        }
    }

    pub fn identity(&self) -> &Identity {
        &self.config.identity
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    pub fn state(&self) -> Arc<Mutex<State>> {
        Arc::clone(&self.state)
    }

    pub fn listener_addr(&self) -> Option<SocketAddr> {
        self.listener_addr
    }

    pub fn set_event_sink(&mut self, sink: Arc<dyn EventSink>) {
        self.event_sink = Some(sink);
    }

    pub fn register_adapter(&mut self, adapter: Arc<dyn DataAdapter>) -> Result<()> {
        let adapter_id = adapter.id().to_string();
        if self.adapters.contains_key(&adapter_id) {
            return Err(Error::Protocol(format!(
                "adapter already registered: {adapter_id}"
            )));
        }
        self.adapters.insert(adapter_id, adapter);
        Ok(())
    }

    pub fn start_listening(&mut self) -> Result<SocketAddr> {
        if self.listener.is_some() {
            return Err(Error::Protocol("listener already running".to_string()));
        }
        let listen_addr = self
            .config
            .listen_addr
            .ok_or_else(|| Error::Protocol("listen address not set".to_string()))?;
        let listener = SyncListener::start(
            listen_addr,
            self.config.identity.clone(),
            Arc::clone(&self.state),
            Arc::clone(&self.device_handler),
        )?;
        let addr = listener.addr();
        self.listener = Some(listener);
        self.listener_addr = Some(addr);
        Ok(addr)
    }

    pub fn stop_listening(&mut self) -> Result<()> {
        let listener = self
            .listener
            .take()
            .ok_or_else(|| Error::Protocol("listener not running".to_string()))?;
        self.listener_addr = None;
        listener.shutdown()?;
        Ok(())
    }

    pub fn discover_devices(&self) -> Result<Vec<DeviceInfo>> {
        not_implemented()
    }

    pub fn add_device(&self, _address: SocketAddr) -> Result<DeviceInfo> {
        not_implemented()
    }

    pub fn request_pair(&self, _device: &DeviceInfo) -> Result<()> {
        not_implemented()
    }

    pub fn respond_to_pairing(
        &self,
        _request: PairingRequest,
        _decision: PairingDecision,
    ) -> Result<()> {
        not_implemented()
    }

    pub fn sync_now(&self, address: SocketAddr, adapter_id: &str) -> Result<DeviceInfo> {
        let adapter = self
            .adapters
            .get(adapter_id)
            .ok_or_else(|| Error::Protocol(format!("missing adapter: {adapter_id}")))?;
        {
            let mut state = self.lock_state()?;
            adapter.load_into_state(&mut state)?;
        }

        let device_check = |identity: &Identity| -> Result<()> {
            if identity.app_id != self.config.identity.app_id {
                return Err(Error::Protocol("app id mismatch".to_string()));
            }
            if !self.device_handler.is_paired(identity) {
                return Err(Error::Protocol("device not paired".to_string()));
            }
            Ok(())
        };

        let remote_identity = {
            let mut state = self.lock_state()?;
            sync_with_device(&self.config.identity, &mut state, address, device_check)?
        };

        {
            let state = self.lock_state()?;
            adapter.apply_from_state(&state)?;
        }

        let device = DeviceInfo {
            identity: remote_identity,
            address: Some(address),
            last_seen: Some(SystemTime::now()),
            paired: true,
        };
        self.emit(Event::SyncFinished {
            device: device.clone(),
            adapter_id: adapter_id.to_string(),
            result: SyncResult::Success,
        });
        Ok(device)
    }

    pub fn watch(&self, _adapter_id: &str) -> Result<()> {
        not_implemented()
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| Error::Protocol("state lock poisoned".to_string()))
    }

    fn emit(&self, event: Event) {
        if let Some(sink) = &self.event_sink {
            sink.emit(event);
        }
    }
}

fn not_implemented<T>() -> Result<T> {
    Err(Error::Protocol(
        "engine API is a sketch and not implemented yet".to_string(),
    ))
}
