use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use flume::{Receiver, Sender};
use sha2::{Digest, Sha256};

use crate::discovery::browse_mdns;
use crate::sync::{pull_snapshot, push_snapshot};
use crate::{
    AdapterCache, DataAdapter, DeviceHandler, Entry, Error, Identity, LogicalAdapter,
    LogicalAdapterWrapper, Result, State, SyncListener, pair_with_device, sync_with_device,
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
    pub fingerprint: Option<String>,
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

#[derive(Clone)]
pub struct EventStream {
    receiver: Receiver<Event>,
}

impl EventStream {
    pub fn recv(&self) -> Result<Event> {
        self.receiver
            .recv()
            .map_err(|_| Error::Protocol("event stream closed".to_string()))
    }

    pub fn try_recv(&self) -> Result<Option<Event>> {
        match self.receiver.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(flume::TryRecvError::Empty) => Ok(None),
            Err(flume::TryRecvError::Disconnected) => {
                Err(Error::Protocol("event stream closed".to_string()))
            }
        }
    }
}

struct EventChannel {
    sender: Sender<Event>,
}

impl EventSink for EventChannel {
    fn emit(&self, event: Event) {
        let _ = self.sender.send(event);
    }
}

pub fn event_channel() -> (Arc<dyn EventSink>, EventStream) {
    let (sender, receiver) = flume::unbounded();
    (
        Arc::new(EventChannel { sender }),
        EventStream { receiver },
    )
}

pub struct AdapterWatch {
    shutdown: mpsc::Sender<()>,
    handle: thread::JoinHandle<Result<()>>,
}

impl AdapterWatch {
    pub fn stop(self) -> Result<()> {
        let _ = self.shutdown.send(());
        match self.handle.join() {
            Ok(result) => result,
            Err(_) => Err(Error::Protocol("adapter watch panicked".to_string())),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AutoRefreshConfig {
    pub adapter_id: String,
    pub state_path: PathBuf,
    pub poll_interval: Duration,
    pub refresh_interval: Option<Duration>,
    pub discover_timeout: Duration,
    pub fallback_addresses: Vec<SocketAddr>,
}

impl AutoRefreshConfig {
    pub fn new(adapter_id: impl Into<String>, state_path: impl Into<PathBuf>) -> Self {
        Self {
            adapter_id: adapter_id.into(),
            state_path: state_path.into(),
            poll_interval: Duration::from_millis(250),
            refresh_interval: Some(Duration::from_secs(5)),
            discover_timeout: Duration::from_secs(2),
            fallback_addresses: Vec::new(),
        }
    }

    pub fn with_poll_interval(mut self, poll_interval: Duration) -> Self {
        self.poll_interval = poll_interval;
        self
    }

    pub fn with_refresh_interval(mut self, refresh_interval: Duration) -> Self {
        self.refresh_interval = Some(refresh_interval);
        self
    }

    pub fn disable_periodic_refresh(mut self) -> Self {
        self.refresh_interval = None;
        self
    }

    pub fn with_discover_timeout(mut self, discover_timeout: Duration) -> Self {
        self.discover_timeout = discover_timeout;
        self
    }

    pub fn with_fallback_addresses(mut self, fallback_addresses: Vec<SocketAddr>) -> Self {
        self.fallback_addresses = fallback_addresses;
        self
    }
}

pub struct AutoRefresh {
    shutdown: mpsc::Sender<()>,
    handle: thread::JoinHandle<Result<()>>,
}

impl AutoRefresh {
    pub fn stop(self) -> Result<()> {
        let _ = self.shutdown.send(());
        match self.handle.join() {
            Ok(result) => result,
            Err(_) => Err(Error::Protocol("auto refresh panicked".to_string())),
        }
    }
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

    pub fn adapter(&self, adapter_id: &str) -> Option<Arc<dyn DataAdapter>> {
        self.adapters.get(adapter_id).cloned()
    }

    pub fn set_event_sink(&mut self, sink: Arc<dyn EventSink>) {
        self.event_sink = Some(sink);
    }

    pub fn attach_event_channel(&mut self) -> EventStream {
        let (sink, stream) = event_channel();
        self.set_event_sink(sink);
        stream
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

    pub fn register_logical_adapter(&mut self, adapter: Arc<dyn LogicalAdapter>) -> Result<()> {
        let wrapper = Arc::new(LogicalAdapterWrapper::new(adapter));
        self.register_adapter(wrapper)
    }

    pub fn start_listening(&mut self) -> Result<SocketAddr> {
        if self.listener.is_some() {
            return Err(Error::Protocol("listener already running".to_string()));
        }
        let _ = self.device_handler.app_key()?;
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
        self.discover_devices_with_timeout(Duration::from_secs(3))
    }

    pub fn discover_devices_with_timeout(&self, timeout: Duration) -> Result<Vec<DeviceInfo>> {
        let discovered = browse_mdns(&self.config.identity.app_id, timeout)?;
        Ok(discovered
            .into_iter()
            .filter(|device| device.identity.device_id != self.config.identity.device_id)
            .map(|device| DeviceInfo {
                paired: self.device_handler.is_paired(&device.identity),
                identity: device.identity,
                address: Some(device.address),
                last_seen: Some(SystemTime::now()),
                fingerprint: None,
            })
            .collect())
    }

    pub fn add_device(&self, _address: SocketAddr) -> Result<DeviceInfo> {
        self.request_pair(_address)
    }

    pub fn request_pair(&self, address: SocketAddr) -> Result<DeviceInfo> {
        let device_keys = self.device_handler.device_keys()?;
        let app_key = self.device_handler.app_key()?;
        let (remote_identity, fingerprint, remote_app_key) =
            pair_with_device(&self.config.identity, &device_keys, &app_key, address)?;
        if remote_app_key != app_key {
            self.device_handler.set_app_key(&remote_app_key)?;
        }

        Ok(DeviceInfo {
            identity: remote_identity,
            address: Some(address),
            last_seen: Some(SystemTime::now()),
            paired: true,
            fingerprint: Some(fingerprint),
        })
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

        let device_keys = self.device_handler.device_keys()?;
        let app_key = self.device_handler.app_key()?;
        let device_check = |identity: &Identity, fingerprint: &str| -> Result<()> {
            if identity.app_id != self.config.identity.app_id {
                return Err(Error::Protocol("app id mismatch".to_string()));
            }
            if !self
                .device_handler
                .is_paired_with_fingerprint(identity, fingerprint)
            {
                return Err(Error::Protocol("device not paired".to_string()));
            }
            Ok(())
        };

        let (remote_identity, fingerprint) = {
            let state = self.lock_state()?;
            push_snapshot(
                &self.config.identity,
                &state,
                address,
                &device_keys,
                &app_key,
                &device_check,
            )?
        };

        let device = DeviceInfo {
            identity: remote_identity.clone(),
            address: Some(address),
            last_seen: Some(SystemTime::now()),
            paired: true,
            fingerprint: Some(fingerprint.clone()),
        };

        self.emit(Event::SyncStarted {
            device: device.clone(),
            adapter_id: adapter_id.to_string(),
        });

        {
            let mut state = self.lock_state()?;
            pull_snapshot(
                &self.config.identity,
                &mut state,
                address,
                &device_keys,
                &app_key,
                &remote_identity,
                &fingerprint,
            )?;
        }

        {
            let state = self.lock_state()?;
            adapter.apply_from_state(&state)?;
        }

        self.emit(Event::SyncFinished {
            device: device.clone(),
            adapter_id: adapter_id.to_string(),
            result: SyncResult::Success,
        });
        Ok(device)
    }

    pub fn watch(
        &self,
        adapter_id: &str,
        state_path: impl Into<PathBuf>,
        interval: Duration,
    ) -> Result<AdapterWatch> {
        let adapter = self
            .adapters
            .get(adapter_id)
            .ok_or_else(|| Error::Protocol(format!("missing adapter: {adapter_id}")))?
            .clone();
        let state = Arc::clone(&self.state);
        let state_path = state_path.into();
        let event_sink = self.event_sink.clone();
        let app_key = self.device_handler.app_key()?;

        let (shutdown_sender, shutdown_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let mut cache = AdapterCache::default();
            loop {
                match shutdown_receiver.recv_timeout(interval) {
                    Ok(()) => break Ok(()),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => break Ok(()),
                }

                let mut state = match state.lock() {
                    Ok(state) => state,
                    Err(_) => {
                        emit_error(&event_sink, "state lock poisoned");
                        continue;
                    }
                };

                if let Err(error) = adapter.sync_tick(&mut state, &mut cache) {
                    emit_error(&event_sink, &format!("adapter sync error: {error}"));
                }

                if let Err(error) = state.save_encrypted(&app_key, &state_path) {
                    emit_error(&event_sink, &format!("state save error: {error}"));
                }
            }
        });

        Ok(AdapterWatch {
            shutdown: shutdown_sender,
            handle,
        })
    }

    pub fn auto_refresh(
        &self,
        adapter_id: &str,
        state_path: impl Into<PathBuf>,
    ) -> Result<AutoRefresh> {
        self.auto_refresh_with_config(AutoRefreshConfig::new(adapter_id, state_path))
    }

    pub fn auto_refresh_with_config(&self, config: AutoRefreshConfig) -> Result<AutoRefresh> {
        let adapter = self
            .adapters
            .get(&config.adapter_id)
            .ok_or_else(|| Error::Protocol(format!("missing adapter: {}", config.adapter_id)))?
            .clone();
        let state = Arc::clone(&self.state);
        let identity = self.config.identity.clone();
        let device_handler = Arc::clone(&self.device_handler);
        let event_sink = self.event_sink.clone();
        let state_path = config.state_path.clone();
        let poll_interval = config.poll_interval;
        let refresh_interval = config.refresh_interval;
        let discover_timeout = config.discover_timeout;
        let fallback_addresses = config.fallback_addresses.clone();
        let adapter_id = config.adapter_id.clone();
        let device_keys = self.device_handler.device_keys()?;
        let app_key = self.device_handler.app_key()?;

        let (shutdown_sender, shutdown_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let mut cache = AdapterCache::default();
            let mut last_digest = None::<[u8; 32]>;
            let mut last_refresh = if let Some(interval) = refresh_interval {
                Instant::now()
                    .checked_sub(interval)
                    .unwrap_or_else(Instant::now)
            } else {
                Instant::now()
            };

            loop {
                match shutdown_receiver.recv_timeout(poll_interval) {
                    Ok(()) => break Ok(()),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => break Ok(()),
                }

                let mut local_changed = false;
                {
                    let mut state = match state.lock() {
                        Ok(state) => state,
                        Err(_) => {
                            emit_error(&event_sink, "state lock poisoned");
                            continue;
                        }
                    };

                    if let Err(error) = adapter.sync_tick(&mut state, &mut cache) {
                        emit_error(&event_sink, &format!("adapter sync error: {error}"));
                    }

                    if let Err(error) = state.save_encrypted(&app_key, &state_path) {
                        emit_error(&event_sink, &format!("state save error: {error}"));
                    }

                    match adapter.export_snapshot(&state) {
                        Ok(entries) => {
                            let digest = hash_entries(entries);
                            if last_digest.as_ref() != Some(&digest) {
                                local_changed = true;
                                last_digest = Some(digest);
                            }
                        }
                        Err(error) => {
                            emit_error(&event_sink, &format!("adapter export error: {error}"));
                        }
                    }
                }

                let interval_due = refresh_interval
                    .map(|interval| last_refresh.elapsed() >= interval)
                    .unwrap_or(false);

                if !local_changed && !interval_due {
                    continue;
                }

                let discovered = match browse_mdns(&identity.app_id, discover_timeout) {
                    Ok(devices) => devices,
                    Err(error) => {
                        emit_error(&event_sink, &format!("discovery error: {error}"));
                        Vec::new()
                    }
                };

                let mut candidates = Vec::new();
                let mut seen = HashSet::new();
                for device in discovered {
                    if device.identity.device_id == identity.device_id {
                        continue;
                    }
                    if !device_handler.is_paired(&device.identity) {
                        continue;
                    }
                    if seen.insert(device.address) {
                        candidates.push((Some(device.identity.clone()), device.address));
                    }
                }
                for address in &fallback_addresses {
                    if seen.insert(*address) {
                        candidates.push((None, *address));
                    }
                }

                let mut synced = false;
                for (known_identity, address) in candidates {
                    let base_identity = known_identity.clone().unwrap_or_else(|| {
                        Identity::new("unknown", identity.app_id.clone(), "unknown")
                    });
                    let device_info_base = DeviceInfo {
                        identity: base_identity,
                        address: Some(address),
                        last_seen: Some(SystemTime::now()),
                        paired: known_identity
                            .as_ref()
                            .map(|identity| device_handler.is_paired(identity))
                            .unwrap_or(false),
                        fingerprint: None,
                    };

                    if let Some(sink) = &event_sink {
                        sink.emit(Event::SyncStarted {
                            device: device_info_base.clone(),
                            adapter_id: adapter_id.clone(),
                        });
                    }

                    let sync_result = {
                        let mut state = match state.lock() {
                            Ok(state) => state,
                            Err(_) => {
                                emit_error(&event_sink, "state lock poisoned");
                                continue;
                            }
                        };

                        if let Err(error) = adapter.load_into_state(&mut state) {
                            emit_error(&event_sink, &format!("adapter load error: {error}"));
                            continue;
                        }

                        let app_id = identity.app_id.clone();
                        let handler = Arc::clone(&device_handler);
                        let app_key = match handler.app_key() {
                            Ok(app_key) => app_key,
                            Err(error) => {
                                emit_error(
                                    &event_sink,
                                    &format!("app key error: {error}"),
                                );
                                continue;
                            }
                        };
                        let mut device_info = device_info_base.clone();
                        match sync_with_device(
                            &identity,
                            &mut state,
                            address,
                            &device_keys,
                            &app_key,
                            |identity, fingerprint| {
                                if identity.app_id != app_id {
                                    return Err(Error::Protocol(
                                        "app id mismatch".to_string(),
                                    ));
                                }
                                if !handler.is_paired_with_fingerprint(identity, fingerprint) {
                                    return Err(Error::Protocol(
                                        "device not paired".to_string(),
                                    ));
                                }
                                Ok(())
                            },
                        ) {
                            Ok((remote_identity, fingerprint)) => {
                                synced = true;
                                device_info.identity = remote_identity;
                                device_info.fingerprint = Some(fingerprint);
                                Ok(device_info)
                            }
                            Err(error) => Err(error),
                        }
                    };

                    match sync_result {
                        Ok(device_info) => {
                            if let Some(sink) = &event_sink {
                                sink.emit(Event::SyncFinished {
                                    device: device_info,
                                    adapter_id: adapter_id.clone(),
                                    result: SyncResult::Success,
                                });
                            }
                        }
                        Err(error) => {
                            if let Some(sink) = &event_sink {
                                sink.emit(Event::SyncFinished {
                                    device: device_info_base,
                                    adapter_id: adapter_id.clone(),
                                    result: SyncResult::Failed(error.to_string()),
                                });
                            }
                        }
                    }
                }

                if synced {
                    let state = match state.lock() {
                        Ok(state) => state,
                        Err(_) => {
                            emit_error(&event_sink, "state lock poisoned");
                            continue;
                        }
                    };
                    if let Err(error) = adapter.apply_from_state(&state) {
                        emit_error(&event_sink, &format!("adapter apply error: {error}"));
                    }
                    if let Err(error) = state.save_encrypted(&app_key, &state_path) {
                        emit_error(&event_sink, &format!("state save error: {error}"));
                    }
                    if let Ok(entries) = adapter.export_snapshot(&state) {
                        last_digest = Some(hash_entries(entries));
                    }
                }

                last_refresh = Instant::now();
            }
        });

        Ok(AutoRefresh {
            shutdown: shutdown_sender,
            handle,
        })
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

fn emit_error(event_sink: &Option<Arc<dyn EventSink>>, message: &str) {
    if let Some(sink) = event_sink {
        sink.emit(Event::Error {
            message: message.to_string(),
        });
    }
}

fn hash_entries(mut entries: Vec<Entry>) -> [u8; 32] {
    entries.sort_by(|left, right| {
        left.key
            .cmp(&right.key)
            .then_with(|| left.clock.cmp(&right.clock))
    });
    let mut hasher = Sha256::new();
    for entry in entries {
        hash_bytes(&mut hasher, entry.key.as_bytes());
        hash_u64(&mut hasher, entry.clock.counter);
        hash_bytes(&mut hasher, entry.clock.device_id.as_bytes());
        hash_bytes(&mut hasher, &entry.value);
    }
    let digest = hasher.finalize();
    let mut output = [0u8; 32];
    output.copy_from_slice(&digest);
    output
}

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    let len = bytes.len() as u64;
    hasher.update(len.to_be_bytes());
    hasher.update(bytes);
}

fn hash_u64(hasher: &mut Sha256, value: u64) {
    hasher.update(value.to_be_bytes());
}

fn not_implemented<T>() -> Result<T> {
    Err(Error::Protocol(
        "engine API is a sketch and not implemented yet".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::{AutoRefreshConfig, Engine, Event, event_channel, hash_entries};
    use crate::{
        AppKey, DeviceHandler, DeviceKeys, EngineConfig, Entry, Identity, JsonFileAdapter,
        LamportClock, LogicalAdapter, RecordState, RecordView, State, SyncListener, SyncRecord,
        FieldValue,
    };
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tempfile::tempdir;

    struct TestHandler {
        app_id: String,
        keys: DeviceKeys,
        app_key: Mutex<AppKey>,
    }

    impl TestHandler {
        fn app_key_value(&self) -> AppKey {
            self.app_key.lock().expect("app key lock").clone()
        }
    }

    impl DeviceHandler for TestHandler {
        fn app_id(&self) -> &str {
            &self.app_id
        }

        fn is_paired(&self, _identity: &Identity) -> bool {
            true
        }

        fn approve_pair(&self, _identity: &Identity) -> crate::Result<bool> {
            Ok(true)
        }

        fn device_keys(&self) -> crate::Result<DeviceKeys> {
            Ok(self.keys.clone())
        }

        fn app_key(&self) -> crate::Result<AppKey> {
            Ok(self.app_key_value())
        }

        fn set_app_key(&self, app_key: &AppKey) -> crate::Result<()> {
            *self.app_key.lock().expect("app key lock") = app_key.clone();
            Ok(())
        }
    }

    struct SimpleLogicalAdapter;

    impl LogicalAdapter for SimpleLogicalAdapter {
        fn id(&self) -> &str {
            "logical"
        }

        fn namespace(&self) -> &str {
            "app"
        }

        fn load_records(&self, records: &mut RecordState) -> crate::Result<()> {
            let record = SyncRecord {
                schema: "schema".to_string(),
                entity: "Todo".to_string(),
                id: "1".to_string(),
                fields: BTreeMap::from([(
                    "title".to_string(),
                    FieldValue::String("hello".to_string()),
                )]),
                tombstone: false,
                clock: crate::LamportClock {
                    counter: 1,
                    device_id: "device".to_string(),
                },
                updated_at: None,
            };
            records.set(record)?;
            Ok(())
        }

        fn apply_records(&self, _records: &RecordView) -> crate::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn register_adapter_rejects_duplicate() {
        let identity = Identity::new("device", "com.example.app", "user");
        let keys = DeviceKeys::generate(&identity).expect("keys");
        let handler = Arc::new(TestHandler {
            app_id: identity.app_id.clone(),
            keys,
            app_key: Mutex::new(AppKey::generate().expect("app key")),
        });
        let mut engine = Engine::new(EngineConfig::new(identity), State::new("device"), handler);
        let adapter = Arc::new(JsonFileAdapter::new("file", "data.json"));
        engine.register_adapter(adapter.clone()).expect("register");
        let err = engine.register_adapter(adapter).expect_err("duplicate");
        assert!(err.to_string().contains("adapter already registered"));
    }

    #[test]
    fn register_logical_adapter_succeeds() {
        let identity = Identity::new("device", "com.example.app", "user");
        let keys = DeviceKeys::generate(&identity).expect("keys");
        let handler = Arc::new(TestHandler {
            app_id: identity.app_id.clone(),
            keys,
            app_key: Mutex::new(AppKey::generate().expect("app key")),
        });
        let mut engine = Engine::new(EngineConfig::new(identity), State::new("device"), handler);
        let adapter = Arc::new(SimpleLogicalAdapter);
        engine
            .register_logical_adapter(adapter)
            .expect("register logical");
    }

    #[test]
    fn request_pair_updates_app_key() {
        let remote_identity = Identity::new("remote", "com.example.app", "user");
        let remote_keys = DeviceKeys::generate(&remote_identity).expect("keys");
        let remote_app_key = AppKey::generate().expect("app key");
        let remote_handler = Arc::new(TestHandler {
            app_id: remote_identity.app_id.clone(),
            keys: remote_keys,
            app_key: Mutex::new(remote_app_key.clone()),
        });
        let remote_state = Arc::new(Mutex::new(State::new("remote")));
        let listener = SyncListener::start(
            "127.0.0.1:0".parse().expect("addr"),
            remote_identity,
            remote_state,
            remote_handler.clone(),
        )
        .expect("listener");

        let local_identity = Identity::new("local", "com.example.app", "user");
        let local_keys = DeviceKeys::generate(&local_identity).expect("keys");
        let local_initial_key = AppKey::generate().expect("app key");
        let local_handler = Arc::new(TestHandler {
            app_id: local_identity.app_id.clone(),
            keys: local_keys,
            app_key: Mutex::new(local_initial_key.clone()),
        });
        let engine =
            Engine::new(EngineConfig::new(local_identity), State::new("local"), local_handler.clone());

        let _ = engine.request_pair(listener.addr()).expect("pair");
        assert_eq!(local_handler.app_key_value(), local_initial_key);
        assert_eq!(remote_handler.app_key_value(), local_initial_key);

        listener.shutdown().expect("shutdown");
    }

    #[test]
    fn event_channel_delivers_and_closes() {
        let (sink, stream) = event_channel();
        sink.emit(Event::Error {
            message: "boom".to_string(),
        });
        match stream.recv().expect("recv") {
            Event::Error { message } => assert_eq!(message, "boom"),
            _ => panic!("unexpected event"),
        }

        drop(sink);
        let err = stream.try_recv().expect_err("closed");
        assert!(err.to_string().contains("event stream closed"));
    }

    #[test]
    fn event_stream_try_recv_empty_returns_none() {
        let (_sink, stream) = event_channel();
        let event = stream.try_recv().expect("try recv");
        assert!(event.is_none());
    }

    #[test]
    fn event_stream_recv_errors_when_closed() {
        let (sink, stream) = event_channel();
        drop(sink);
        let err = stream.recv().expect_err("closed");
        assert!(err.to_string().contains("event stream closed"));
    }

    #[test]
    fn auto_refresh_config_builders_set_fields() {
        let config = AutoRefreshConfig::new("file", "state.json")
            .with_poll_interval(Duration::from_secs(1))
            .with_refresh_interval(Duration::from_secs(2))
            .with_discover_timeout(Duration::from_secs(3))
            .with_fallback_addresses(vec!["127.0.0.1:5555".parse().expect("addr")]);
        assert_eq!(config.adapter_id, "file");
        assert_eq!(config.poll_interval, Duration::from_secs(1));
        assert_eq!(config.refresh_interval, Some(Duration::from_secs(2)));
        assert_eq!(config.discover_timeout, Duration::from_secs(3));
        assert_eq!(config.fallback_addresses.len(), 1);

        let config = AutoRefreshConfig::new("file", "state.json").disable_periodic_refresh();
        assert_eq!(config.refresh_interval, None);
    }

    #[test]
    fn engine_start_and_stop_listening() {
        let identity = Identity::new("device", "com.example.app", "user");
        let keys = DeviceKeys::generate(&identity).expect("keys");
        let handler = Arc::new(TestHandler {
            app_id: identity.app_id.clone(),
            keys,
            app_key: Mutex::new(AppKey::generate().expect("app key")),
        });
        let config = EngineConfig::new(identity).with_listen_addr(
            "127.0.0.1:0".parse().expect("addr"),
        );
        let mut engine = Engine::new(config, State::new("device"), handler);

        let addr = engine.start_listening().expect("start");
        assert!(addr.port() > 0);
        assert!(engine.listener_addr().is_some());
        assert!(engine.start_listening().is_err());

        engine.stop_listening().expect("stop");
        assert!(engine.listener_addr().is_none());
        assert!(engine.stop_listening().is_err());
    }

    #[test]
    fn engine_start_listening_requires_address() {
        let identity = Identity::new("device", "com.example.app", "user");
        let keys = DeviceKeys::generate(&identity).expect("keys");
        let handler = Arc::new(TestHandler {
            app_id: identity.app_id.clone(),
            keys,
            app_key: Mutex::new(AppKey::generate().expect("app key")),
        });
        let mut engine = Engine::new(EngineConfig::new(identity), State::new("device"), handler);

        let result = engine.start_listening();
        assert!(result.is_err());
    }

    #[test]
    fn engine_watch_errors_without_adapter() {
        let identity = Identity::new("device", "com.example.app", "user");
        let keys = DeviceKeys::generate(&identity).expect("keys");
        let handler = Arc::new(TestHandler {
            app_id: identity.app_id.clone(),
            keys,
            app_key: Mutex::new(AppKey::generate().expect("app key")),
        });
        let engine = Engine::new(EngineConfig::new(identity), State::new("device"), handler);
        let result = engine.watch("missing", "state.json", Duration::from_millis(10));
        assert!(result.is_err());
    }

    #[test]
    fn engine_auto_refresh_errors_without_adapter() {
        let identity = Identity::new("device", "com.example.app", "user");
        let keys = DeviceKeys::generate(&identity).expect("keys");
        let handler = Arc::new(TestHandler {
            app_id: identity.app_id.clone(),
            keys,
            app_key: Mutex::new(AppKey::generate().expect("app key")),
        });
        let engine = Engine::new(EngineConfig::new(identity), State::new("device"), handler);
        let config = AutoRefreshConfig::new("missing", "state.json");
        let result = engine.auto_refresh_with_config(config);
        assert!(result.is_err());
    }

    #[test]
    fn watch_writes_state_file() {
        let temp = tempdir().expect("tempdir");
        let data_path = temp.path().join("data.json");
        std::fs::write(&data_path, b"{}").expect("write data");

        let identity = Identity::new("device", "com.example.app", "user");
        let keys = DeviceKeys::generate(&identity).expect("keys");
        let handler = Arc::new(TestHandler {
            app_id: identity.app_id.clone(),
            keys,
            app_key: Mutex::new(AppKey::generate().expect("app key")),
        });
        let mut engine = Engine::new(EngineConfig::new(identity), State::new("device"), handler);
        let adapter = Arc::new(JsonFileAdapter::new("file", &data_path));
        engine.register_adapter(adapter).expect("register");

        let state_path = temp.path().join("state.json");
        let watch = engine
            .watch("file", &state_path, Duration::from_millis(10))
            .expect("watch");
        std::thread::sleep(Duration::from_millis(30));
        watch.stop().expect("stop");

        assert!(state_path.exists());
    }

    #[test]
    fn hash_entries_is_order_insensitive() {
        let entry_a = Entry {
            key: "a".to_string(),
            value: b"one".to_vec(),
            clock: LamportClock {
                counter: 1,
                device_id: "device".to_string(),
            },
        };
        let entry_b = Entry {
            key: "b".to_string(),
            value: b"two".to_vec(),
            clock: LamportClock {
                counter: 2,
                device_id: "device".to_string(),
            },
        };

        let hash1 = hash_entries(vec![entry_a.clone(), entry_b.clone()]);
        let hash2 = hash_entries(vec![entry_b, entry_a]);
        assert_eq!(hash1, hash2);
    }
}
