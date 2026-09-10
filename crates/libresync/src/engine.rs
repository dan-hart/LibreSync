use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use flume::{Receiver, Sender};
use sha2::{Digest, Sha256};

use crate::discovery::{discover_devices, DEFAULT_SYNC_PORT};
use crate::sync::{sync_with_device_shared, ListenerOptions, SyncOptions, SyncStats};
use crate::{
    link_with_device_using, AdapterRouter, CancelToken, DataAdapter, DeviceHandler, Entry, Error,
    Identity, LogicalAdapter, LogicalAdapterWrapper, Result, State, SyncListener,
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
    pub linked: bool,
    pub fingerprint: Option<String>,
}

#[derive(Clone, Debug)]
pub struct LinkingRequest {
    pub device: DeviceInfo,
    pub code: Option<String>,
}

#[derive(Clone, Debug)]
pub enum LinkingDecision {
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
    LinkingRequested {
        request: LinkingRequest,
    },
    LinkingDecisionRequired {
        request: LinkingRequest,
    },
    SyncStarted {
        device: DeviceInfo,
        adapter_id: String,
    },
    SyncFinished {
        device: DeviceInfo,
        adapter_id: String,
        result: SyncResult,
    },
    DeviceSeen {
        device: DeviceInfo,
    },
    DeviceOffline {
        device: DeviceInfo,
    },
    Error {
        message: String,
    },
    /// A linked device presented a certificate that does not match the
    /// fingerprint pinned at link time. The connection was rejected; the app
    /// should ask the user whether to re-link. `device.fingerprint` and
    /// `fingerprint` both carry the newly presented value.
    FingerprintChanged {
        device: DeviceInfo,
        fingerprint: String,
    },
    /// A peer connected to the local listener and its entries were merged.
    /// `applied` counts entries that changed local state.
    InboundSync {
        device: DeviceInfo,
        applied: usize,
    },
    /// A sync exchange completed; carries traffic counters.
    SyncStats {
        device: DeviceInfo,
        adapter_id: String,
        stats: SyncStats,
    },
    /// A background link request finished.
    LinkFinished {
        address: SocketAddr,
        device: Option<DeviceInfo>,
        error: Option<String>,
    },
    /// A background discovery pass finished.
    DiscoveryFinished {
        devices: Vec<DeviceInfo>,
    },
    ListenerStarted {
        address: SocketAddr,
    },
    ListenerStopped,
    /// A command submitted to a [`BackgroundEngine`] finished.
    TaskFinished {
        ticket: u64,
        result: SyncResult,
    },
}

pub trait EventSink: Send + Sync {
    fn emit(&self, event: Event);
}

/// Receiver side of the engine event channel.
///
/// Besides blocking and non-blocking receives, the stream can wake a GUI
/// main loop without polling:
///
/// - [`EventStream::raw_fd`] returns a file descriptor that becomes readable
///   whenever events are queued (glib: `g_unix_fd_add`; CFRunLoop / GCD:
///   `DispatchSource.makeReadSource`). Drain with `try_recv` until `None`.
/// - [`EventStream::set_waker`] registers a callback invoked on the emitting
///   thread every time an event is queued (e.g. `glib::idle_add_once` or
///   `DispatchQueue.main.async`).
#[derive(Clone)]
pub struct EventStream {
    receiver: Receiver<Event>,
    notifier: Arc<Notifier>,
}

impl EventStream {
    pub fn recv(&self) -> Result<Event> {
        let event = self
            .receiver
            .recv()
            .map_err(|_| Error::Protocol("event stream closed".to_string()))?;
        self.notifier.after_take(&self.receiver);
        Ok(event)
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<Event> {
        let event = self
            .receiver
            .recv_timeout(timeout)
            .map_err(|error| match error {
                flume::RecvTimeoutError::Timeout => {
                    Error::Protocol("event stream timeout".to_string())
                }
                flume::RecvTimeoutError::Disconnected => {
                    Error::Protocol("event stream closed".to_string())
                }
            })?;
        self.notifier.after_take(&self.receiver);
        Ok(event)
    }

    pub fn try_recv(&self) -> Result<Option<Event>> {
        loop {
            match self.receiver.try_recv() {
                Ok(event) => {
                    self.notifier.after_take(&self.receiver);
                    return Ok(Some(event));
                }
                Err(flume::TryRecvError::Empty) => {
                    if self.notifier.consume_signal() {
                        // A wake byte was pending; re-check in case an event
                        // landed between the two operations.
                        continue;
                    }
                    return Ok(None);
                }
                Err(flume::TryRecvError::Disconnected) => {
                    return Err(Error::Protocol("event stream closed".to_string()))
                }
            }
        }
    }

    /// Number of queued events.
    pub fn len(&self) -> usize {
        self.receiver.len()
    }

    pub fn is_empty(&self) -> bool {
        self.receiver.is_empty()
    }

    /// File descriptor that is readable while events are queued.
    ///
    /// Do not read from it directly: call [`EventStream::try_recv`] until it
    /// returns `None`, which also clears the readiness. Returns `None` on
    /// platforms without pipes.
    #[cfg(unix)]
    pub fn raw_fd(&self) -> Option<std::os::fd::RawFd> {
        use std::os::fd::AsRawFd;
        self.notifier
            .pipe
            .as_ref()
            .and_then(|pipe| pipe.reader.lock().ok().map(|reader| reader.as_raw_fd()))
    }

    #[cfg(not(unix))]
    pub fn raw_fd(&self) -> Option<i32> {
        None
    }

    /// Registers a callback invoked (on the emitting thread) after each event
    /// is queued. Replaces any previous waker.
    pub fn set_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        if let Ok(mut guard) = self.notifier.waker.lock() {
            *guard = Some(Arc::new(waker));
        }
    }

    pub fn clear_waker(&self) {
        if let Ok(mut guard) = self.notifier.waker.lock() {
            *guard = None;
        }
    }
}

type Waker = Arc<dyn Fn() + Send + Sync>;

struct Notifier {
    signaled: AtomicBool,
    waker: Mutex<Option<Waker>>,
    #[cfg(unix)]
    pipe: Option<WakePipe>,
}

#[cfg(unix)]
struct WakePipe {
    reader: Mutex<std::io::PipeReader>,
    writer: Mutex<std::io::PipeWriter>,
}

impl Notifier {
    fn new() -> Self {
        Self {
            signaled: AtomicBool::new(false),
            waker: Mutex::new(None),
            #[cfg(unix)]
            pipe: std::io::pipe().ok().map(|(reader, writer)| WakePipe {
                reader: Mutex::new(reader),
                writer: Mutex::new(writer),
            }),
        }
    }

    /// Called after an event was queued. Keeps the invariant "exactly one
    /// unread wake byte while `signaled` is set".
    fn signal(&self) {
        if !self.signaled.swap(true, Ordering::SeqCst) {
            #[cfg(unix)]
            if let Some(pipe) = &self.pipe {
                if let Ok(mut writer) = pipe.writer.lock() {
                    let _ = writer.write_all(&[1]);
                }
            }
        }
        let waker = self.waker.lock().ok().and_then(|guard| guard.clone());
        if let Some(waker) = waker {
            waker();
        }
    }

    /// Clears the pending wake byte, if any. Returns whether one was pending.
    fn consume_signal(&self) -> bool {
        if !self.signaled.swap(false, Ordering::SeqCst) {
            return false;
        }
        #[cfg(unix)]
        if let Some(pipe) = &self.pipe {
            if let Ok(mut reader) = pipe.reader.lock() {
                let mut byte = [0u8; 1];
                let _ = reader.read_exact(&mut byte);
            }
        }
        true
    }

    /// After an event was taken: re-arm the wake byte when more events are
    /// queued, so consumers that take one event per wake-up are not starved.
    fn after_take(&self, receiver: &Receiver<Event>) {
        if receiver.is_empty() {
            self.consume_signal();
        } else if !self.signaled.load(Ordering::SeqCst) {
            self.signal();
        }
    }
}

struct EventChannel {
    sender: Sender<Event>,
    notifier: Arc<Notifier>,
}

impl EventSink for EventChannel {
    fn emit(&self, event: Event) {
        if self.sender.send(event).is_ok() {
            self.notifier.signal();
        }
    }
}

pub fn event_channel() -> (Arc<dyn EventSink>, EventStream) {
    let (sender, receiver) = flume::unbounded();
    let notifier = Arc::new(Notifier::new());
    (
        Arc::new(EventChannel {
            sender,
            notifier: Arc::clone(&notifier),
        }),
        EventStream { receiver, notifier },
    )
}

struct FanoutSink {
    sinks: Vec<Arc<dyn EventSink>>,
}

impl EventSink for FanoutSink {
    fn emit(&self, event: Event) {
        for sink in &self.sinks {
            sink.emit(event.clone());
        }
    }
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

/// Describes one sync exchange with a peer.
#[derive(Clone, Debug)]
pub struct SyncRequest {
    pub address: SocketAddr,
    pub adapter_id: String,
    /// Pin the peer's certificate fingerprint for the TLS handshake.
    pub expected_fingerprint: Option<String>,
    /// Expected device id (used as the TLS server name when valid).
    pub expected_device_id: Option<String>,
    pub cancel: Option<CancelToken>,
}

impl SyncRequest {
    pub fn new(address: SocketAddr, adapter_id: impl Into<String>) -> Self {
        Self {
            address,
            adapter_id: adapter_id.into(),
            expected_fingerprint: None,
            expected_device_id: None,
            cancel: None,
        }
    }

    /// Builds a request that pins the fingerprint and identity recorded in
    /// `device`. Fails when the device has no address.
    pub fn for_device(device: &DeviceInfo, adapter_id: impl Into<String>) -> Result<Self> {
        let address = device
            .address
            .ok_or_else(|| Error::Protocol("device has no address".to_string()))?;
        Ok(Self {
            address,
            adapter_id: adapter_id.into(),
            expected_fingerprint: device.fingerprint.clone(),
            expected_device_id: Some(device.identity.device_id.clone()),
            cancel: None,
        })
    }

    pub fn with_expected_fingerprint(mut self, fingerprint: impl Into<String>) -> Self {
        self.expected_fingerprint = Some(fingerprint.into());
        self
    }

    pub fn with_expected_device_id(mut self, device_id: impl Into<String>) -> Self {
        self.expected_device_id = Some(device_id.into());
        self
    }

    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = Some(cancel);
        self
    }
}

pub struct Engine {
    config: EngineConfig,
    state: Arc<Mutex<State>>,
    adapters: Arc<Mutex<HashMap<String, Arc<dyn DataAdapter>>>>,
    event_sink: Option<Arc<dyn EventSink>>,
    listener: Option<SyncListener>,
    listener_addr: Option<SocketAddr>,
    device_handler: Arc<dyn DeviceHandler>,
}

impl Engine {
    pub fn new(config: EngineConfig, state: State, device_handler: Arc<dyn DeviceHandler>) -> Self {
        Self {
            config,
            state: Arc::new(Mutex::new(state)),
            adapters: Arc::new(Mutex::new(HashMap::new())),
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
        self.router().adapter(adapter_id)
    }

    /// Router that applies inbound entries through the registered adapters.
    pub fn router(&self) -> AdapterRouter {
        AdapterRouter::from_shared(Arc::clone(&self.adapters))
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
        self.router().register(adapter)
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
        let mut options = ListenerOptions::default().with_applier(Arc::new(self.router()));
        if let Some(sink) = &self.event_sink {
            options = options.with_event_sink(Arc::clone(sink));
        }
        let listener = SyncListener::start_with_options(
            listen_addr,
            self.config.identity.clone(),
            Arc::clone(&self.state),
            Arc::clone(&self.device_handler),
            options,
        )?;
        let addr = listener.addr();
        self.listener = Some(listener);
        self.listener_addr = Some(addr);
        self.emit(Event::ListenerStarted { address: addr });
        Ok(addr)
    }

    pub fn stop_listening(&mut self) -> Result<()> {
        let listener = self
            .listener
            .take()
            .ok_or_else(|| Error::Protocol("listener not running".to_string()))?;
        self.listener_addr = None;
        listener.shutdown()?;
        self.emit(Event::ListenerStopped);
        Ok(())
    }

    pub fn discover_devices(&self) -> Result<Vec<DeviceInfo>> {
        self.discover_devices_with_timeout(Duration::from_secs(3))
    }

    pub fn discover_devices_with_timeout(&self, timeout: Duration) -> Result<Vec<DeviceInfo>> {
        let overlay_port = self
            .listener_addr
            .or(self.config.listen_addr)
            .map(|addr| addr.port())
            .unwrap_or(DEFAULT_SYNC_PORT);
        let discovered = discover_devices(&self.config.identity.app_id, timeout, overlay_port)?;
        Ok(discovered
            .into_iter()
            .filter(|device| device.identity.device_id != self.config.identity.device_id)
            .map(|device| DeviceInfo {
                linked: self.device_handler.is_linked(&device.identity),
                identity: device.identity,
                address: Some(device.address),
                last_seen: Some(SystemTime::now()),
                fingerprint: None,
            })
            .collect())
    }

    pub fn add_device(&self, _address: SocketAddr) -> Result<DeviceInfo> {
        self.request_link(_address)
    }

    pub fn request_link(&self, address: SocketAddr) -> Result<DeviceInfo> {
        self.request_link_cancellable(address, None)
    }

    pub fn request_link_cancellable(
        &self,
        address: SocketAddr,
        cancel: Option<CancelToken>,
    ) -> Result<DeviceInfo> {
        let device_keys = self.device_handler.device_keys()?;
        let app_key = self.device_handler.app_key()?;
        let pairing_secret = self.device_handler.pairing_secret();
        let mut options = SyncOptions::default();
        if let Some(cancel) = cancel {
            options = options.with_cancel(cancel);
        }
        let (remote_identity, fingerprint, remote_app_key) = link_with_device_using(
            &self.config.identity,
            &device_keys,
            &app_key,
            address,
            pairing_secret,
            &options,
        )?;
        if remote_app_key != app_key {
            self.device_handler.set_app_key(&remote_app_key)?;
        }

        Ok(DeviceInfo {
            identity: remote_identity,
            address: Some(address),
            last_seen: Some(SystemTime::now()),
            linked: true,
            fingerprint: Some(fingerprint),
        })
    }

    pub fn respond_to_linking(
        &self,
        _request: LinkingRequest,
        _decision: LinkingDecision,
    ) -> Result<()> {
        not_implemented()
    }

    /// Syncs one adapter with the peer at `address`. Inbound entries are
    /// merged through the registered adapters (field-level policies for
    /// logical adapters). Blocks for the duration of the exchange; drive it
    /// from a background thread or use [`Engine::spawn`] in GUIs.
    pub fn sync_now(&self, address: SocketAddr, adapter_id: &str) -> Result<DeviceInfo> {
        self.sync(&SyncRequest::new(address, adapter_id))
            .map(|(device, _)| device)
    }

    pub fn sync_now_with_stats(
        &self,
        address: SocketAddr,
        adapter_id: &str,
    ) -> Result<(DeviceInfo, SyncStats)> {
        self.sync(&SyncRequest::new(address, adapter_id))
    }

    /// Syncs with a known device, pinning its recorded fingerprint for the
    /// TLS handshake.
    pub fn sync_with_device_info(
        &self,
        device: &DeviceInfo,
        adapter_id: &str,
    ) -> Result<(DeviceInfo, SyncStats)> {
        self.sync(&SyncRequest::for_device(device, adapter_id)?)
    }

    pub fn sync(&self, request: &SyncRequest) -> Result<(DeviceInfo, SyncStats)> {
        let adapter = self
            .adapter(&request.adapter_id)
            .ok_or_else(|| Error::Protocol(format!("missing adapter: {}", request.adapter_id)))?;
        {
            let mut state = self.lock_state()?;
            adapter.load_into_state(&mut state)?;
        }

        let device_keys = self.device_handler.device_keys()?;
        let app_key = self.device_handler.app_key()?;
        let adapter_id = request.adapter_id.clone();
        let address = request.address;
        let device_check = |identity: &Identity, fingerprint: &str| -> Result<()> {
            self.check_peer(identity, fingerprint, Some(address))?;
            self.emit(Event::SyncStarted {
                device: DeviceInfo {
                    identity: identity.clone(),
                    address: Some(address),
                    last_seen: Some(SystemTime::now()),
                    linked: true,
                    fingerprint: Some(fingerprint.to_string()),
                },
                adapter_id: adapter_id.clone(),
            });
            Ok(())
        };

        let mut options = SyncOptions::default().with_applier(Arc::new(self.router()));
        if let Some(fingerprint) = &request.expected_fingerprint {
            options = options.with_expected_fingerprint(fingerprint.clone());
        }
        if let Some(device_id) = &request.expected_device_id {
            options = options.with_expected_device_id(device_id.clone());
        }
        if let Some(cancel) = &request.cancel {
            options = options.with_cancel(cancel.clone());
        }

        let outcome = match sync_with_device_shared(
            &self.config.identity,
            &self.state,
            address,
            &device_keys,
            &app_key,
            device_check,
            &options,
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                if let Some(device_id) = &request.expected_device_id {
                    self.emit(Event::SyncFinished {
                        device: DeviceInfo {
                            identity: Identity::new(
                                device_id.clone(),
                                self.config.identity.app_id.clone(),
                                "unknown",
                            ),
                            address: Some(address),
                            last_seen: None,
                            linked: true,
                            fingerprint: request.expected_fingerprint.clone(),
                        },
                        adapter_id: request.adapter_id.clone(),
                        result: SyncResult::Failed(error.to_string()),
                    });
                }
                return Err(error);
            }
        };

        let device = DeviceInfo {
            identity: outcome.identity,
            address: Some(address),
            last_seen: Some(SystemTime::now()),
            linked: true,
            fingerprint: Some(outcome.fingerprint),
        };

        {
            let state = self.lock_state()?;
            adapter.apply_from_state(&state)?;
        }

        self.emit(Event::SyncFinished {
            device: device.clone(),
            adapter_id: request.adapter_id.clone(),
            result: SyncResult::Success,
        });
        self.emit(Event::SyncStats {
            device: device.clone(),
            adapter_id: request.adapter_id.clone(),
            stats: outcome.stats.clone(),
        });
        Ok((device, outcome.stats))
    }

    /// Verifies a peer after the TLS handshake: app id, linking, and pinned
    /// fingerprint. Emits [`Event::FingerprintChanged`] when a linked device
    /// presents a different certificate.
    fn check_peer(
        &self,
        identity: &Identity,
        fingerprint: &str,
        address: Option<SocketAddr>,
    ) -> Result<()> {
        check_peer_with_handler(
            self.device_handler.as_ref(),
            &self.event_sink,
            &self.config.identity.app_id,
            identity,
            fingerprint,
            address,
        )
    }

    /// Moves the engine onto a background thread. See [`BackgroundEngine`].
    pub fn spawn(self) -> Result<BackgroundEngine> {
        BackgroundEngine::spawn(self)
    }

    pub fn watch(
        &self,
        adapter_id: &str,
        state_path: impl Into<PathBuf>,
        interval: Duration,
    ) -> Result<AdapterWatch> {
        let adapter = self
            .adapter(adapter_id)
            .ok_or_else(|| Error::Protocol(format!("missing adapter: {adapter_id}")))?;
        let state = Arc::clone(&self.state);
        let state_path = state_path.into();
        let event_sink = self.event_sink.clone();
        let app_key = self.device_handler.app_key()?;

        let (shutdown_sender, shutdown_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let mut cache = crate::AdapterCache::default();
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
            .adapter(&config.adapter_id)
            .ok_or_else(|| Error::Protocol(format!("missing adapter: {}", config.adapter_id)))?;
        let state = Arc::clone(&self.state);
        let identity = self.config.identity.clone();
        let device_handler = Arc::clone(&self.device_handler);
        let event_sink = self.event_sink.clone();
        let router = self.router();
        let state_path = config.state_path.clone();
        let poll_interval = config.poll_interval;
        let refresh_interval = config.refresh_interval;
        let discover_timeout = config.discover_timeout;
        let fallback_addresses = config.fallback_addresses.clone();
        let adapter_id = config.adapter_id.clone();
        let overlay_port = self
            .listener_addr
            .or(self.config.listen_addr)
            .map(|addr| addr.port())
            .unwrap_or(DEFAULT_SYNC_PORT);
        let device_keys = self.device_handler.device_keys()?;
        let app_key = self.device_handler.app_key()?;

        let (shutdown_sender, shutdown_receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let mut cache = crate::AdapterCache::default();
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

                let discovered =
                    match discover_devices(&identity.app_id, discover_timeout, overlay_port) {
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
                    if !device_handler.is_linked(&device.identity) {
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
                        linked: known_identity
                            .as_ref()
                            .map(|identity| device_handler.is_linked(identity))
                            .unwrap_or(false),
                        fingerprint: None,
                    };

                    if let Some(sink) = &event_sink {
                        sink.emit(Event::SyncStarted {
                            device: device_info_base.clone(),
                            adapter_id: adapter_id.clone(),
                        });
                    }

                    {
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
                    }

                    let app_id = identity.app_id.clone();
                    let handler = Arc::clone(&device_handler);
                    let app_key = match handler.app_key() {
                        Ok(app_key) => app_key,
                        Err(error) => {
                            emit_error(&event_sink, &format!("app key error: {error}"));
                            continue;
                        }
                    };
                    let mut device_info = device_info_base.clone();
                    let mut options = SyncOptions::default().with_applier(Arc::new(router.clone()));
                    if let Some(known) = &known_identity {
                        options = options.with_expected_device_id(known.device_id.clone());
                    }
                    let sink_for_check = event_sink.clone();
                    let sync_result = sync_with_device_shared(
                        &identity,
                        &state,
                        address,
                        &device_keys,
                        &app_key,
                        |remote, fingerprint| {
                            check_peer_with_handler(
                                handler.as_ref(),
                                &sink_for_check,
                                &app_id,
                                remote,
                                fingerprint,
                                Some(address),
                            )
                        },
                        &options,
                    )
                    .map(|outcome| {
                        synced = true;
                        device_info.identity = outcome.identity;
                        device_info.fingerprint = Some(outcome.fingerprint);
                        (device_info, outcome.stats)
                    });

                    match sync_result {
                        Ok((device_info, stats)) => {
                            if let Some(sink) = &event_sink {
                                sink.emit(Event::SyncFinished {
                                    device: device_info.clone(),
                                    adapter_id: adapter_id.clone(),
                                    result: SyncResult::Success,
                                });
                                sink.emit(Event::SyncStats {
                                    device: device_info,
                                    adapter_id: adapter_id.clone(),
                                    stats,
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

fn check_peer_with_handler(
    handler: &dyn DeviceHandler,
    event_sink: &Option<Arc<dyn EventSink>>,
    app_id: &str,
    identity: &Identity,
    fingerprint: &str,
    address: Option<SocketAddr>,
) -> Result<()> {
    if identity.app_id != app_id {
        return Err(Error::Protocol("app id mismatch".to_string()));
    }
    if handler.is_linked_with_fingerprint(identity, fingerprint) {
        return Ok(());
    }
    if handler.is_linked(identity) {
        if let Some(sink) = event_sink {
            sink.emit(Event::FingerprintChanged {
                device: DeviceInfo {
                    identity: identity.clone(),
                    address,
                    last_seen: Some(SystemTime::now()),
                    linked: true,
                    fingerprint: Some(fingerprint.to_string()),
                },
                fingerprint: fingerprint.to_string(),
            });
        }
        return Err(Error::FingerprintMismatch {
            device_id: identity.device_id.clone(),
            expected: "<pinned>".to_string(),
            actual: fingerprint.to_string(),
        });
    }
    Err(Error::Protocol("device not linked".to_string()))
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

// ---------------------------------------------------------------------------
// Background engine
// ---------------------------------------------------------------------------

/// Ticket identifying a command submitted to a [`BackgroundEngine`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ticket(pub u64);

type EngineJob = Box<dyn FnOnce(&mut Engine) -> Result<()> + Send>;

enum Command {
    Sync { ticket: u64, request: SyncRequest },
    Link { ticket: u64, address: SocketAddr },
    Discover { ticket: u64, timeout: Duration },
    StartListening { ticket: u64 },
    StopListening { ticket: u64 },
    SaveState { ticket: u64, path: PathBuf },
    Run { ticket: u64, job: EngineJob },
    Shutdown,
}

impl Command {
    fn ticket(&self) -> Option<u64> {
        match self {
            Command::Sync { ticket, .. }
            | Command::Link { ticket, .. }
            | Command::Discover { ticket, .. }
            | Command::StartListening { ticket }
            | Command::StopListening { ticket }
            | Command::SaveState { ticket, .. }
            | Command::Run { ticket, .. } => Some(*ticket),
            Command::Shutdown => None,
        }
    }
}

/// An [`Engine`] driven from a background thread.
///
/// Every method returns immediately; results arrive as [`Event`]s on
/// [`BackgroundEngine::events`], which a GUI can integrate through
/// [`EventStream::raw_fd`] or [`EventStream::set_waker`] without polling.
/// Commands run one at a time in submission order; the current one can be
/// cancelled with [`BackgroundEngine::cancel`], queued ones are skipped.
pub struct BackgroundEngine {
    sender: Mutex<Option<mpsc::Sender<Command>>>,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
    events: EventStream,
    next_ticket: AtomicU64,
    current: Arc<Mutex<Option<(u64, CancelToken)>>>,
    cancelled: Arc<Mutex<HashSet<u64>>>,
    state: Arc<Mutex<State>>,
    identity: Identity,
    listener_addr: Arc<Mutex<Option<SocketAddr>>>,
}

impl BackgroundEngine {
    fn spawn(mut engine: Engine) -> Result<Self> {
        let (sink, events) = event_channel();
        let sink: Arc<dyn EventSink> = match engine.event_sink.take() {
            Some(existing) => Arc::new(FanoutSink {
                sinks: vec![existing, sink],
            }),
            None => sink,
        };
        engine.set_event_sink(Arc::clone(&sink));

        let (sender, receiver) = mpsc::channel::<Command>();
        let current: Arc<Mutex<Option<(u64, CancelToken)>>> = Arc::new(Mutex::new(None));
        let cancelled: Arc<Mutex<HashSet<u64>>> = Arc::new(Mutex::new(HashSet::new()));
        let state = engine.state();
        let identity = engine.identity().clone();
        let listener_addr = Arc::new(Mutex::new(engine.listener_addr()));

        let loop_current = Arc::clone(&current);
        let loop_cancelled = Arc::clone(&cancelled);
        let loop_listener_addr = Arc::clone(&listener_addr);
        let thread = thread::spawn(move || {
            while let Ok(command) = receiver.recv() {
                let ticket = match command.ticket() {
                    Some(ticket) => ticket,
                    None => break,
                };
                let skipped = loop_cancelled
                    .lock()
                    .map(|mut set| set.remove(&ticket))
                    .unwrap_or(false);
                if skipped {
                    sink.emit(Event::TaskFinished {
                        ticket,
                        result: SyncResult::Failed(Error::Cancelled.to_string()),
                    });
                    continue;
                }
                let cancel = CancelToken::new();
                if let Ok(mut guard) = loop_current.lock() {
                    *guard = Some((ticket, cancel.clone()));
                }
                let result = run_command(&mut engine, command, cancel, &loop_listener_addr, &sink);
                if let Ok(mut guard) = loop_current.lock() {
                    *guard = None;
                }
                sink.emit(Event::TaskFinished {
                    ticket,
                    result: match result {
                        Ok(()) => SyncResult::Success,
                        Err(error) => SyncResult::Failed(error.to_string()),
                    },
                });
            }
            if let Some(listener) = engine.listener.take() {
                let _ = listener.shutdown();
            }
        });

        Ok(Self {
            sender: Mutex::new(Some(sender)),
            thread: Mutex::new(Some(thread)),
            events,
            next_ticket: AtomicU64::new(1),
            current,
            cancelled,
            state,
            identity,
            listener_addr,
        })
    }

    pub fn events(&self) -> EventStream {
        self.events.clone()
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    pub fn state(&self) -> Arc<Mutex<State>> {
        Arc::clone(&self.state)
    }

    pub fn listener_addr(&self) -> Option<SocketAddr> {
        self.listener_addr.lock().ok().and_then(|guard| *guard)
    }

    fn submit(&self, build: impl FnOnce(u64) -> Command) -> Result<Ticket> {
        let ticket = self.next_ticket.fetch_add(1, Ordering::SeqCst);
        let sender = self
            .sender
            .lock()
            .map_err(|_| Error::Protocol("background engine poisoned".to_string()))?;
        match sender.as_ref() {
            Some(sender) => sender
                .send(build(ticket))
                .map_err(|_| Error::Protocol("background engine stopped".to_string()))?,
            None => return Err(Error::Protocol("background engine stopped".to_string())),
        }
        Ok(Ticket(ticket))
    }

    /// Queues a sync; returns immediately. Completion is reported by
    /// `Event::SyncFinished` (and `Event::TaskFinished` with the ticket).
    pub fn try_sync_now(&self, address: SocketAddr, adapter_id: &str) -> Result<Ticket> {
        self.try_sync(SyncRequest::new(address, adapter_id))
    }

    pub fn try_sync(&self, request: SyncRequest) -> Result<Ticket> {
        self.submit(|ticket| Command::Sync { ticket, request })
    }

    pub fn try_request_link(&self, address: SocketAddr) -> Result<Ticket> {
        self.submit(|ticket| Command::Link { ticket, address })
    }

    pub fn try_discover(&self, timeout: Duration) -> Result<Ticket> {
        self.submit(|ticket| Command::Discover { ticket, timeout })
    }

    pub fn try_start_listening(&self) -> Result<Ticket> {
        self.submit(|ticket| Command::StartListening { ticket })
    }

    pub fn try_stop_listening(&self) -> Result<Ticket> {
        self.submit(|ticket| Command::StopListening { ticket })
    }

    pub fn try_save_state(&self, path: impl Into<PathBuf>) -> Result<Ticket> {
        let path = path.into();
        self.submit(|ticket| Command::SaveState { ticket, path })
    }

    /// Runs an arbitrary job on the engine thread (register adapters, attach
    /// watches, ...).
    pub fn run(
        &self,
        job: impl FnOnce(&mut Engine) -> Result<()> + Send + 'static,
    ) -> Result<Ticket> {
        self.submit(|ticket| Command::Run {
            ticket,
            job: Box::new(job),
        })
    }

    /// Cancels a queued or running command.
    pub fn cancel(&self, ticket: Ticket) {
        if let Ok(guard) = self.current.lock() {
            if let Some((current, cancel)) = guard.as_ref() {
                if *current == ticket.0 {
                    cancel.cancel();
                    return;
                }
            }
        }
        if let Ok(mut set) = self.cancelled.lock() {
            set.insert(ticket.0);
        }
    }

    /// Cancels the running command.
    pub fn cancel_current(&self) {
        if let Ok(guard) = self.current.lock() {
            if let Some((_, cancel)) = guard.as_ref() {
                cancel.cancel();
            }
        }
    }

    /// Stops the background thread, cancelling the running command and
    /// shutting down the listener. Queued commands are dropped.
    pub fn shutdown(&self) -> Result<()> {
        self.cancel_current();
        if let Ok(mut sender) = self.sender.lock() {
            if let Some(sender) = sender.take() {
                let _ = sender.send(Command::Shutdown);
            }
        }
        let thread = self.thread.lock().ok().and_then(|mut guard| guard.take());
        match thread {
            Some(thread) => thread
                .join()
                .map_err(|_| Error::Protocol("background engine panicked".to_string())),
            None => Ok(()),
        }
    }
}

impl Drop for BackgroundEngine {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn run_command(
    engine: &mut Engine,
    command: Command,
    cancel: CancelToken,
    listener_addr: &Arc<Mutex<Option<SocketAddr>>>,
    sink: &Arc<dyn EventSink>,
) -> Result<()> {
    match command {
        Command::Sync { mut request, .. } => {
            request.cancel = Some(cancel);
            engine.sync(&request).map(|_| ())
        }
        Command::Link { address, .. } => {
            let result = engine.request_link_cancellable(address, Some(cancel));
            sink.emit(Event::LinkFinished {
                address,
                device: result.as_ref().ok().cloned(),
                error: result.as_ref().err().map(|error| error.to_string()),
            });
            result.map(|_| ())
        }
        Command::Discover { timeout, .. } => {
            let devices = engine.discover_devices_with_timeout(timeout)?;
            for device in &devices {
                sink.emit(Event::DeviceSeen {
                    device: device.clone(),
                });
            }
            sink.emit(Event::DiscoveryFinished { devices });
            Ok(())
        }
        Command::StartListening { .. } => {
            let address = engine.start_listening()?;
            if let Ok(mut guard) = listener_addr.lock() {
                *guard = Some(address);
            }
            Ok(())
        }
        Command::StopListening { .. } => {
            engine.stop_listening()?;
            if let Ok(mut guard) = listener_addr.lock() {
                *guard = None;
            }
            Ok(())
        }
        Command::SaveState { path, .. } => {
            let app_key = engine.device_handler.app_key()?;
            let state = engine.lock_state()?;
            state.save_encrypted(&app_key, path)
        }
        Command::Run { job, .. } => job(engine),
        Command::Shutdown => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::{event_channel, hash_entries, AutoRefreshConfig, Engine, Event, SyncRequest};
    use crate::{
        AppKey, DeviceHandler, DeviceKeys, EngineConfig, Entry, FieldValue, Identity,
        InMemoryLogicalAdapter, JsonFileAdapter, LamportClock, LogicalAdapter, MergePolicy,
        RecordState, RecordView, State, SyncListener, SyncRecord, SyncResult,
    };
    use std::collections::{BTreeMap, HashMap};
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

        fn is_linked(&self, _identity: &Identity) -> bool {
            true
        }

        fn approve_link(&self, _identity: &Identity) -> crate::Result<bool> {
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
                field_clocks: Default::default(),
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
    fn request_link_updates_app_key() {
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
        let engine = Engine::new(
            EngineConfig::new(local_identity),
            State::new("local"),
            local_handler.clone(),
        );

        let _ = engine.request_link(listener.addr()).expect("link");
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
        let config =
            EngineConfig::new(identity).with_listen_addr("127.0.0.1:0".parse().expect("addr"));
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

    /// Handler that pins fingerprints per device id, like a real app would.
    struct PinningHandler {
        app_id: String,
        keys: DeviceKeys,
        app_key: AppKey,
        pins: Mutex<HashMap<String, String>>,
    }

    impl PinningHandler {
        fn new(identity: &Identity, app_key: &AppKey) -> Arc<Self> {
            Arc::new(Self {
                app_id: identity.app_id.clone(),
                keys: DeviceKeys::generate(identity).expect("keys"),
                app_key: app_key.clone(),
                pins: Mutex::new(HashMap::new()),
            })
        }

        fn pin(&self, device_id: &str, fingerprint: &str) {
            self.pins
                .lock()
                .expect("pins")
                .insert(device_id.to_string(), fingerprint.to_string());
        }
    }

    impl DeviceHandler for PinningHandler {
        fn app_id(&self) -> &str {
            &self.app_id
        }

        fn is_linked(&self, identity: &Identity) -> bool {
            self.pins
                .lock()
                .expect("pins")
                .contains_key(&identity.device_id)
        }

        fn approve_link(&self, _identity: &Identity) -> crate::Result<bool> {
            Ok(true)
        }

        fn device_keys(&self) -> crate::Result<DeviceKeys> {
            Ok(self.keys.clone())
        }

        fn app_key(&self) -> crate::Result<AppKey> {
            Ok(self.app_key.clone())
        }

        fn is_linked_with_fingerprint(&self, identity: &Identity, fingerprint: &str) -> bool {
            self.pins
                .lock()
                .expect("pins")
                .get(&identity.device_id)
                .map(|pinned| pinned == fingerprint)
                .unwrap_or(false)
        }

        fn approve_link_with_fingerprint(
            &self,
            identity: &Identity,
            fingerprint: &str,
        ) -> crate::Result<bool> {
            self.pin(&identity.device_id, fingerprint);
            Ok(true)
        }
    }

    fn todo(
        id: &str,
        counter: u64,
        device: &str,
        fields: BTreeMap<String, FieldValue>,
    ) -> SyncRecord {
        SyncRecord {
            schema: "todos".to_string(),
            entity: "Todo".to_string(),
            id: id.to_string(),
            fields,
            tombstone: false,
            clock: LamportClock {
                counter,
                device_id: device.to_string(),
            },
            updated_at: None,
            field_clocks: Default::default(),
        }
    }

    fn logical_engine(
        device_id: &str,
        app_key: &AppKey,
        listen: bool,
    ) -> (Engine, Arc<InMemoryLogicalAdapter>, Arc<PinningHandler>) {
        let identity = Identity::new(device_id, "com.example.app", "user");
        let handler = PinningHandler::new(&identity, app_key);
        let mut config = EngineConfig::new(identity);
        if listen {
            config = config.with_listen_addr("127.0.0.1:0".parse().expect("addr"));
        }
        let mut engine = Engine::new(config, State::new(device_id), handler.clone());
        let adapter = Arc::new(
            InMemoryLogicalAdapter::new("todos", "app")
                .with_merge_policy("tags", MergePolicy::SetUnion),
        );
        engine
            .register_logical_adapter(adapter.clone())
            .expect("register");
        (engine, adapter, handler)
    }

    #[test]
    fn concurrent_edits_to_different_fields_both_survive_sync() {
        let app_key = AppKey::generate().expect("app key");
        let (mut engine_a, adapter_a, handler_a) = logical_engine("device-a", &app_key, true);
        let (engine_b, adapter_b, handler_b) = logical_engine("device-b", &app_key, false);
        handler_a.pin("device-b", handler_b.keys.fingerprint());
        handler_b.pin("device-a", handler_a.keys.fingerprint());
        let events = engine_a.attach_event_channel();
        let addr = engine_a.start_listening().expect("listen");

        // Shared starting point on both devices.
        let base = BTreeMap::from([
            (
                "title".to_string(),
                FieldValue::String("Buy milk".to_string()),
            ),
            ("done".to_string(), FieldValue::Bool(false)),
            (
                "tags".to_string(),
                FieldValue::List(vec![FieldValue::String("home".to_string())]),
            ),
        ]);
        adapter_a.upsert_record(todo("1", 1, "device-a", base.clone()));
        let (_, stats) = engine_b
            .sync_now_with_stats(addr, "todos")
            .expect("initial sync");
        assert_eq!(stats.entries_received, 1);
        assert_eq!(
            adapter_b
                .record("todos", "Todo", "1")
                .expect("record")
                .fields,
            base
        );
        while events.try_recv().expect("events").is_some() {}

        // Concurrent, disjoint edits: A renames and adds a tag, B completes it.
        let mut edit_a = base.clone();
        edit_a.insert(
            "title".to_string(),
            FieldValue::String("Buy oat milk".to_string()),
        );
        edit_a.insert(
            "tags".to_string(),
            FieldValue::List(vec![
                FieldValue::String("home".to_string()),
                FieldValue::String("urgent".to_string()),
            ]),
        );
        adapter_a.upsert_record(todo("1", 2, "device-a", edit_a));
        let mut edit_b = base.clone();
        edit_b.insert("done".to_string(), FieldValue::Bool(true));
        edit_b.insert(
            "tags".to_string(),
            FieldValue::List(vec![
                FieldValue::String("home".to_string()),
                FieldValue::String("shopping".to_string()),
            ]),
        );
        adapter_b.upsert_record(todo("1", 3, "device-b", edit_b));

        // B syncs with A (A is the listener): both directions merge per field.
        let (device, stats) = engine_b
            .sync_now_with_stats(addr, "todos")
            .expect("sync b -> a");
        assert_eq!(device.identity.device_id, "device-a");
        assert_eq!(stats.entries_received, 1);

        for adapter in [&adapter_a, &adapter_b] {
            let record = adapter.record("todos", "Todo", "1").expect("record");
            assert_eq!(
                record.fields.get("title"),
                Some(&FieldValue::String("Buy oat milk".to_string())),
                "A's title edit survived"
            );
            assert_eq!(
                record.fields.get("done"),
                Some(&FieldValue::Bool(true)),
                "B's completion survived"
            );
            let tags = match record.fields.get("tags") {
                Some(FieldValue::List(items)) => items.clone(),
                other => panic!("unexpected tags {other:?}"),
            };
            for tag in ["home", "urgent", "shopping"] {
                assert!(tags.contains(&FieldValue::String(tag.to_string())), "{tag}");
            }
        }

        // The listener reported the inbound merge.
        let mut inbound = false;
        while let Ok(event) = events.recv_timeout(Duration::from_secs(1)) {
            if let Event::InboundSync { device, applied } = event {
                assert_eq!(device.identity.device_id, "device-b");
                assert_eq!(applied, 1);
                inbound = true;
                break;
            }
        }
        assert!(inbound);

        // A second sync is idle: the merged record is not echoed back.
        let (_, stats) = engine_b.sync_now_with_stats(addr, "todos").expect("idle");
        assert_eq!(stats.entries_sent, 0);
        assert_eq!(stats.entries_received, 0);

        engine_a.stop_listening().expect("stop");
    }

    #[test]
    fn engine_reports_fingerprint_change_and_pins_for_known_devices() {
        let app_key = AppKey::generate().expect("app key");
        let (mut engine_a, _, handler_a) = logical_engine("device-a", &app_key, true);
        let (mut engine_b, _, handler_b) = logical_engine("device-b", &app_key, false);
        let addr = engine_a.start_listening().expect("listen");
        handler_a.pin("device-b", handler_b.keys.fingerprint());
        let events_b = engine_b.attach_event_channel();

        // B linked A with a fingerprint that no longer matches (A rotated).
        let stale = DeviceKeys::generate(engine_a.identity()).expect("stale keys");
        handler_b.pin("device-a", stale.fingerprint());
        let error = engine_b.sync_now(addr, "todos").expect_err("mismatch");
        assert!(
            matches!(error, crate::Error::FingerprintMismatch { .. }),
            "{error}"
        );
        let mut changed = None;
        while let Ok(event) = events_b.recv_timeout(Duration::from_secs(1)) {
            if let Event::FingerprintChanged {
                device,
                fingerprint,
            } = event
            {
                changed = Some((device, fingerprint));
                break;
            }
        }
        let (device, fingerprint) = changed.expect("fingerprint changed event");
        assert_eq!(device.identity.device_id, "device-a");
        assert_eq!(fingerprint, handler_a.keys.fingerprint());

        // Re-pinning (the user accepted the change) makes pinned syncs work.
        handler_b.pin("device-a", handler_a.keys.fingerprint());
        let request = SyncRequest::for_device(&device, "todos").expect("request");
        let (synced, _) = engine_b.sync(&request).expect("pinned sync");
        assert_eq!(
            synced.fingerprint.as_deref(),
            Some(handler_a.keys.fingerprint())
        );

        // A stale explicit pin fails the TLS handshake before any data flows.
        let request = SyncRequest::new(addr, "todos")
            .with_expected_fingerprint(stale.fingerprint())
            .with_expected_device_id("device-a");
        let error = engine_b.sync(&request).expect_err("handshake mismatch");
        assert!(
            matches!(error, crate::Error::FingerprintMismatch { .. }),
            "{error}"
        );
        let mut failed = false;
        while let Ok(event) = events_b.recv_timeout(Duration::from_secs(1)) {
            if let Event::SyncFinished {
                result: SyncResult::Failed(_),
                ..
            } = event
            {
                failed = true;
                break;
            }
        }
        assert!(failed);
        assert!(SyncRequest::for_device(
            &crate::DeviceInfo {
                identity: Identity::new("x", "com.example.app", "u"),
                address: None,
                last_seen: None,
                linked: false,
                fingerprint: None,
            },
            "todos"
        )
        .is_err());

        engine_a.stop_listening().expect("stop");
    }

    #[test]
    fn background_engine_runs_commands_and_reports_events() {
        let app_key = AppKey::generate().expect("app key");
        let (mut engine_a, adapter_a, handler_a) = logical_engine("device-a", &app_key, true);
        let (engine_b, adapter_b, handler_b) = logical_engine("device-b", &app_key, false);
        handler_a.pin("device-b", handler_b.keys.fingerprint());
        handler_b.pin("device-a", handler_a.keys.fingerprint());
        let addr = engine_a.start_listening().expect("listen");
        adapter_a.upsert_record(todo(
            "1",
            1,
            "device-a",
            BTreeMap::from([("title".to_string(), FieldValue::String("hi".to_string()))]),
        ));

        let background = engine_b.spawn().expect("spawn");
        let events = background.events();
        assert!(events.raw_fd().is_some() || !cfg!(unix));
        assert_eq!(background.identity().device_id, "device-b");
        assert!(background.listener_addr().is_none());

        let ticket = background.try_sync_now(addr, "todos").expect("queue sync");
        let temp = tempdir().expect("tempdir");
        let state_path = temp.path().join("state.enc");
        let save = background.try_save_state(&state_path).expect("queue save");
        let ran = Arc::new(Mutex::new(false));
        let flag = Arc::clone(&ran);
        let job = background
            .run(move |engine| {
                *flag.lock().expect("flag") = true;
                assert!(engine.adapter("todos").is_some());
                Ok(())
            })
            .expect("queue job");

        let mut finished = std::collections::HashSet::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while finished.len() < 3 && std::time::Instant::now() < deadline {
            if let Ok(Event::TaskFinished { ticket, result }) =
                events.recv_timeout(Duration::from_secs(5))
            {
                assert!(matches!(result, SyncResult::Success), "{result:?}");
                finished.insert(ticket);
            }
        }
        assert_eq!(finished, [ticket.0, save.0, job.0].into_iter().collect());
        assert!(adapter_b.record("todos", "Todo", "1").is_some());
        assert!(state_path.exists());
        assert!(*ran.lock().expect("flag"));

        // Cancelling a queued command skips it.
        let blocked = background
            .run(|_| {
                std::thread::sleep(Duration::from_millis(300));
                Ok(())
            })
            .expect("queue sleep");
        let skipped = background
            .try_discover(Duration::from_millis(10))
            .expect("queue");
        background.cancel(skipped);
        let mut outcomes = HashMap::new();
        while outcomes.len() < 2 {
            if let Ok(Event::TaskFinished { ticket, result }) =
                events.recv_timeout(Duration::from_secs(5))
            {
                outcomes.insert(ticket, result);
            } else {
                break;
            }
        }
        assert!(matches!(
            outcomes.get(&blocked.0),
            Some(SyncResult::Success)
        ));
        assert!(matches!(
            outcomes.get(&skipped.0),
            Some(SyncResult::Failed(_))
        ));

        background.shutdown().expect("shutdown");
        assert!(background.try_sync_now(addr, "todos").is_err());
        engine_a.stop_listening().expect("stop");
    }

    #[test]
    fn background_engine_cancels_running_sync() {
        // A listener that accepts and stalls.
        let stall = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = stall.local_addr().expect("addr");
        std::thread::spawn(move || {
            let (stream, _) = stall.accept().expect("accept");
            std::thread::sleep(Duration::from_secs(3));
            drop(stream);
        });
        let app_key = AppKey::generate().expect("app key");
        let (engine, _, _) = logical_engine("device-b", &app_key, false);
        let background = engine.spawn().expect("spawn");
        let events = background.events();
        let ticket = background.try_sync_now(addr, "todos").expect("queue");
        std::thread::sleep(Duration::from_millis(200));
        let started = std::time::Instant::now();
        background.cancel(ticket);
        let mut result = None;
        while let Ok(event) = events.recv_timeout(Duration::from_secs(5)) {
            if let Event::TaskFinished {
                ticket: done,
                result: outcome,
            } = event
            {
                if done == ticket.0 {
                    result = Some(outcome);
                    break;
                }
            }
        }
        assert!(matches!(result, Some(SyncResult::Failed(_))), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(3));
        background.cancel_current();
        drop(background);
    }

    #[test]
    fn event_stream_fd_and_waker_signal_queued_events() {
        let (sink, stream) = event_channel();
        let woken = Arc::new(Mutex::new(0usize));
        let counter = Arc::clone(&woken);
        stream.set_waker(move || *counter.lock().expect("counter") += 1);
        assert!(stream.is_empty());
        sink.emit(Event::ListenerStopped);
        sink.emit(Event::ListenerStopped);
        assert_eq!(stream.len(), 2);
        assert_eq!(*woken.lock().expect("counter"), 2);

        #[cfg(unix)]
        {
            let fd = stream.raw_fd().expect("fd");
            assert!(fd >= 0);
            assert!(
                fd_readable(fd),
                "fd should be readable while events are queued"
            );
        }

        assert!(stream.try_recv().expect("recv").is_some());
        #[cfg(unix)]
        assert!(
            fd_readable(stream.raw_fd().expect("fd")),
            "re-armed with one event left"
        );
        assert!(stream.try_recv().expect("recv").is_some());
        assert!(stream.try_recv().expect("recv").is_none());
        #[cfg(unix)]
        assert!(!fd_readable(stream.raw_fd().expect("fd")), "drained");

        sink.emit(Event::ListenerStopped);
        #[cfg(unix)]
        assert!(fd_readable(stream.raw_fd().expect("fd")));
        assert!(stream.recv_timeout(Duration::from_secs(1)).is_ok());
        assert!(stream.recv_timeout(Duration::from_millis(10)).is_err());
        stream.clear_waker();
        sink.emit(Event::ListenerStopped);
        assert_eq!(*woken.lock().expect("counter"), 3);
        let event = stream.recv().expect("recv");
        assert!(matches!(event, Event::ListenerStopped));
    }

    /// Readiness probe using poll(2), the same primitive glib and CFRunLoop
    /// use to watch a descriptor. Does not consume the wake byte.
    #[cfg(unix)]
    fn fd_readable(fd: std::os::fd::RawFd) -> bool {
        #[repr(C)]
        struct PollFd {
            fd: i32,
            events: i16,
            revents: i16,
        }
        #[cfg(target_os = "linux")]
        type NfdsT = std::os::raw::c_ulong;
        #[cfg(not(target_os = "linux"))]
        type NfdsT = std::os::raw::c_uint;
        extern "C" {
            fn poll(fds: *mut PollFd, nfds: NfdsT, timeout: i32) -> i32;
        }
        const POLLIN: i16 = 0x0001;
        let mut pollfd = PollFd {
            fd,
            events: POLLIN,
            revents: 0,
        };
        let ready = unsafe { poll(&mut pollfd, 1, 0) };
        ready > 0 && (pollfd.revents & POLLIN) != 0
    }
}
