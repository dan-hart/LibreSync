use std::io::{BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, DistinguishedName, ServerConfig,
    ServerConnection, SignatureScheme, StreamOwned,
};

use crate::engine::{DeviceInfo, Event, EventSink};
use crate::protocol::{read_message, write_message, Message, PROTOCOL_VERSION};
use crate::{
    decrypt_entries, encrypt_entries, AppKey, DeviceHandler, DeviceKeys, Error, Identity,
    InboundApplier, LwwApplier, PeerCursor, Result, State,
};

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_SNI: &str = "libresync.local";

/// Cooperative cancellation for a running sync or link operation.
///
/// Cancelling flips a flag checked between protocol steps and shuts down the
/// sockets registered by the operation, so a peer that stopped responding does
/// not hold the caller until the socket timeout expires.
#[derive(Clone, Default)]
pub struct CancelToken {
    inner: Arc<CancelInner>,
}

impl std::fmt::Debug for CancelToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CancelToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

#[derive(Default)]
struct CancelInner {
    cancelled: AtomicBool,
    sockets: Mutex<Vec<TcpStream>>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::SeqCst);
        if let Ok(mut sockets) = self.inner.sockets.lock() {
            for socket in sockets.drain(..) {
                let _ = socket.shutdown(Shutdown::Both);
            }
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// Clears the cancelled flag so the token can be reused.
    pub fn reset(&self) {
        self.inner.cancelled.store(false, Ordering::SeqCst);
    }

    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }

    fn register(&self, stream: &TcpStream) -> Result<()> {
        let clone = stream.try_clone()?;
        if let Ok(mut sockets) = self.inner.sockets.lock() {
            sockets.push(clone);
        }
        self.check()
    }

    fn release(&self) {
        if let Ok(mut sockets) = self.inner.sockets.lock() {
            sockets.clear();
        }
    }
}

/// Traffic and entry counters for one sync exchange.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncStats {
    /// Entries this device sent to the peer.
    pub entries_sent: usize,
    /// Entries received from the peer (before merge).
    pub entries_received: usize,
    /// Entries received that changed local state after merging.
    pub entries_applied: usize,
    /// TLS bytes written to the socket(s).
    pub bytes_sent: u64,
    /// TLS bytes read from the socket(s).
    pub bytes_received: u64,
    /// True when either direction fell back to a full snapshot.
    pub full_snapshot: bool,
    /// Protocol version negotiated with the peer.
    pub protocol_version: u32,
}

/// Result of a completed sync exchange.
#[derive(Clone, Debug)]
pub struct SyncOutcome {
    pub identity: Identity,
    pub fingerprint: String,
    pub stats: SyncStats,
}

/// Options for [`sync_with_device_using`].
#[derive(Clone)]
pub struct SyncOptions {
    /// How inbound entries are merged. Defaults to whole-entry
    /// last-writer-wins; the engine passes an [`crate::AdapterRouter`].
    pub applier: Arc<dyn InboundApplier>,
    /// Pinned certificate fingerprint of the peer. When set, the TLS
    /// handshake itself fails if the peer presents another leaf certificate.
    pub expected_fingerprint: Option<String>,
    /// Expected device id, used as the TLS server name when it is a valid DNS
    /// label.
    pub expected_device_id: Option<String>,
    pub cancel: Option<CancelToken>,
    pub connect_timeout: Duration,
    pub io_timeout: Duration,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            applier: Arc::new(LwwApplier),
            expected_fingerprint: None,
            expected_device_id: None,
            cancel: None,
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            io_timeout: DEFAULT_IO_TIMEOUT,
        }
    }
}

impl SyncOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_applier(mut self, applier: Arc<dyn InboundApplier>) -> Self {
        self.applier = applier;
        self
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

    pub fn with_timeouts(mut self, connect: Duration, io: Duration) -> Self {
        self.connect_timeout = connect;
        self.io_timeout = io;
        self
    }

    fn check_cancel(&self) -> Result<()> {
        match &self.cancel {
            Some(cancel) => cancel.check(),
            None => Ok(()),
        }
    }
}

/// Options for [`SyncListener::start_with_options`].
#[derive(Clone)]
pub struct ListenerOptions {
    pub applier: Arc<dyn InboundApplier>,
    pub event_sink: Option<Arc<dyn EventSink>>,
    pub io_timeout: Duration,
}

impl Default for ListenerOptions {
    fn default() -> Self {
        Self {
            applier: Arc::new(LwwApplier),
            event_sink: None,
            io_timeout: DEFAULT_IO_TIMEOUT,
        }
    }
}

impl ListenerOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_applier(mut self, applier: Arc<dyn InboundApplier>) -> Self {
        self.applier = applier;
        self
    }

    pub fn with_event_sink(mut self, sink: Arc<dyn EventSink>) -> Self {
        self.event_sink = Some(sink);
        self
    }

    pub fn with_io_timeout(mut self, io_timeout: Duration) -> Self {
        self.io_timeout = io_timeout;
        self
    }
}

struct ListenerShared {
    identity: Identity,
    handler: Arc<dyn DeviceHandler>,
    state: Arc<Mutex<State>>,
    device_keys: DeviceKeys,
    options: ListenerOptions,
    stopping: AtomicBool,
}

/// Accepts inbound link and sync connections on a background thread.
///
/// The accept loop blocks (no polling); `shutdown` wakes it with a loopback
/// connection. Each accepted connection is served on its own thread.
pub struct SyncListener {
    addr: SocketAddr,
    shared: Arc<ListenerShared>,
    handle: thread::JoinHandle<Result<()>>,
}

impl SyncListener {
    pub fn start(
        addr: SocketAddr,
        identity: Identity,
        state: Arc<Mutex<State>>,
        handler: Arc<dyn DeviceHandler>,
    ) -> Result<Self> {
        Self::start_with_options(addr, identity, state, handler, ListenerOptions::default())
    }

    pub fn start_with_options(
        addr: SocketAddr,
        identity: Identity,
        state: Arc<Mutex<State>>,
        handler: Arc<dyn DeviceHandler>,
        options: ListenerOptions,
    ) -> Result<Self> {
        let listener = TcpListener::bind(addr)?;
        let local_addr = listener.local_addr()?;
        let device_keys = handler.device_keys()?;
        let shared = Arc::new(ListenerShared {
            identity,
            handler,
            state,
            device_keys,
            options,
            stopping: AtomicBool::new(false),
        });

        let loop_shared = Arc::clone(&shared);
        let handle = thread::spawn(move || loop {
            let (stream, _) = match listener.accept() {
                Ok(accepted) => accepted,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => break Err(error.into()),
            };
            if loop_shared.stopping.load(Ordering::SeqCst) {
                break Ok(());
            }
            let conn_shared = Arc::clone(&loop_shared);
            thread::spawn(move || {
                if let Err(error) = handle_connection(stream, &conn_shared) {
                    if let Some(sink) = &conn_shared.options.event_sink {
                        sink.emit(Event::Error {
                            message: format!("inbound connection error: {error}"),
                        });
                    }
                    #[cfg(test)]
                    eprintln!("sync listener error: {error}");
                }
            });
        });

        Ok(Self {
            addr: local_addr,
            shared,
            handle,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn shutdown(self) -> Result<()> {
        self.shared.stopping.store(true, Ordering::SeqCst);
        // Wake the blocking accept with a loopback connection.
        let wake_addr = wake_address(self.addr);
        let _ = TcpStream::connect_timeout(&wake_addr, Duration::from_secs(1));
        match self.handle.join() {
            Ok(result) => result,
            Err(_) => Err(Error::Protocol("sync listener panicked".to_string())),
        }
    }
}

fn wake_address(addr: SocketAddr) -> SocketAddr {
    let ip = match addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    SocketAddr::new(ip, addr.port())
}

/// Syncs with a peer using whole-entry last-writer-wins merges.
///
/// Prefer [`sync_with_device_using`] with an [`crate::AdapterRouter`] so
/// logical adapters apply their field-level merge policies.
pub fn sync_with_device<F>(
    identity: &Identity,
    state: &mut State,
    device: SocketAddr,
    device_keys: &DeviceKeys,
    app_key: &AppKey,
    device_check: F,
) -> Result<(Identity, String)>
where
    F: Fn(&Identity, &str) -> Result<()>,
{
    let outcome = sync_with_device_using(
        identity,
        state,
        device,
        device_keys,
        app_key,
        device_check,
        &SyncOptions::default(),
    )?;
    Ok((outcome.identity, outcome.fingerprint))
}

/// Syncs with a peer over a single connection, exchanging deltas in both
/// directions and merging inbound entries through `options.applier`.
pub fn sync_with_device_using<F>(
    identity: &Identity,
    state: &mut State,
    device: SocketAddr,
    device_keys: &DeviceKeys,
    app_key: &AppKey,
    device_check: F,
    options: &SyncOptions,
) -> Result<SyncOutcome>
where
    F: Fn(&Identity, &str) -> Result<()>,
{
    run_sync(
        identity,
        &mut DirectState(state),
        device,
        device_keys,
        app_key,
        &device_check,
        options,
    )
}

/// Same as [`sync_with_device_using`] but locks a shared state only while it
/// is read or written, never across network waits.
pub fn sync_with_device_shared<F>(
    identity: &Identity,
    state: &Arc<Mutex<State>>,
    device: SocketAddr,
    device_keys: &DeviceKeys,
    app_key: &AppKey,
    device_check: F,
    options: &SyncOptions,
) -> Result<SyncOutcome>
where
    F: Fn(&Identity, &str) -> Result<()>,
{
    run_sync(
        identity,
        &mut SharedState(state),
        device,
        device_keys,
        app_key,
        &device_check,
        options,
    )
}

pub(crate) trait StateAccess {
    fn with_state<R>(&mut self, f: impl FnOnce(&mut State) -> Result<R>) -> Result<R>;
}

struct DirectState<'a>(&'a mut State);

impl StateAccess for DirectState<'_> {
    fn with_state<R>(&mut self, f: impl FnOnce(&mut State) -> Result<R>) -> Result<R> {
        f(self.0)
    }
}

struct SharedState<'a>(&'a Arc<Mutex<State>>);

impl StateAccess for SharedState<'_> {
    fn with_state<R>(&mut self, f: impl FnOnce(&mut State) -> Result<R>) -> Result<R> {
        let mut guard = self
            .0
            .lock()
            .map_err(|_| Error::Protocol("state lock poisoned".to_string()))?;
        f(&mut guard)
    }
}

pub(crate) fn run_sync<A: StateAccess>(
    identity: &Identity,
    state: &mut A,
    device: SocketAddr,
    device_keys: &DeviceKeys,
    app_key: &AppKey,
    device_check: &dyn Fn(&Identity, &str) -> Result<()>,
    options: &SyncOptions,
) -> Result<SyncOutcome> {
    let result = run_sync_inner(
        identity,
        state,
        device,
        device_keys,
        app_key,
        device_check,
        options,
    );
    if let Some(cancel) = &options.cancel {
        cancel.release();
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
    }
    result
}

fn run_sync_inner<A: StateAccess>(
    identity: &Identity,
    state: &mut A,
    device: SocketAddr,
    device_keys: &DeviceKeys,
    app_key: &AppKey,
    device_check: &dyn Fn(&Identity, &str) -> Result<()>,
    options: &SyncOptions,
) -> Result<SyncOutcome> {
    options.check_cancel()?;
    let counters = ByteCounters::default();
    let mut reader = open_client(device, device_keys, options, &counters)?;

    write_message(reader.get_mut(), &Message::hello(identity.clone()))?;
    let (remote_identity, remote_version) = read_hello(&mut reader, identity)?;
    let fingerprint = device_fingerprint(reader.get_mut().conn.peer_certificates())?;
    if let Some(expected) = &options.expected_fingerprint {
        if *expected != fingerprint {
            return Err(Error::FingerprintMismatch {
                device_id: remote_identity.device_id,
                expected: expected.clone(),
                actual: fingerprint,
            });
        }
    }
    device_check(&remote_identity, &fingerprint)?;
    options.check_cancel()?;

    let mut stats = SyncStats {
        protocol_version: remote_version.min(PROTOCOL_VERSION),
        ..SyncStats::default()
    };

    if remote_version < 2 {
        // Legacy peer: push the full snapshot here, then pull on a second
        // connection.
        let snapshot = state.with_state(|state| Ok(state.snapshot()))?;
        stats.entries_sent = snapshot.len();
        stats.full_snapshot = true;
        let encrypted = encrypt_entries(app_key, snapshot)?;
        write_message(reader.get_mut(), &Message::Snapshot { entries: encrypted })?;
        if read_message(&mut reader)? != Message::Ack {
            return Err(Error::Protocol("expected ack".to_string()));
        }
        drop(reader);
        options.check_cancel()?;

        let mut reader = open_client(device, device_keys, options, &counters)?;
        write_message(reader.get_mut(), &Message::hello(identity.clone()))?;
        let (pull_identity, _) = read_hello(&mut reader, identity)?;
        if pull_identity != remote_identity {
            return Err(Error::Protocol("device identity changed".to_string()));
        }
        let pull_fingerprint = device_fingerprint(reader.get_mut().conn.peer_certificates())?;
        if pull_fingerprint != fingerprint {
            return Err(Error::Protocol("device fingerprint changed".to_string()));
        }
        write_message(reader.get_mut(), &Message::SnapshotRequest)?;
        let entries = match read_message(&mut reader)? {
            Message::Snapshot { entries } => entries,
            _ => return Err(Error::Protocol("expected snapshot".to_string())),
        };
        let entries = decrypt_entries(app_key, entries)?;
        stats.entries_received = entries.len();
        let remote_id = remote_identity.device_id.clone();
        stats.entries_applied = state.with_state(|state| {
            state.with_inbound_origin(&remote_id, |state| {
                options.applier.apply_inbound(state, entries)
            })
        })?;
        counters.fill(&mut stats);
        return Ok(SyncOutcome {
            identity: remote_identity,
            fingerprint,
            stats,
        });
    }

    let remote_id = remote_identity.device_id.clone();
    let cursor = state.with_state(|state| Ok(state.peer_cursor(&remote_id)))?;
    write_message(
        reader.get_mut(),
        &Message::SnapshotSince {
            clock: cursor.clock,
            epoch: cursor.epoch,
        },
    )?;

    let (entries, remote_cursor, acked, full) = match read_message(&mut reader)? {
        Message::Delta {
            entries,
            clock,
            epoch,
            acked,
            acked_epoch,
            full,
        } => (
            entries,
            PeerCursor { epoch, clock },
            PeerCursor {
                epoch: acked_epoch,
                clock: acked,
            },
            full,
        ),
        _ => return Err(Error::Protocol("expected delta".to_string())),
    };
    options.check_cancel()?;
    let entries = decrypt_entries(app_key, entries)?;
    stats.entries_received = entries.len();
    stats.full_snapshot |= full;

    // Compute the outbound delta before merging inbound entries so nothing
    // received in this exchange is echoed back.
    let (outbound, local_cursor, outbound_full, applied) = state.with_state(|state| {
        let since = state.resolve_cursor(&acked);
        let outbound_full = state.is_full_snapshot_request(since);
        let outbound = if outbound_full {
            state.snapshot()
        } else {
            state.delta_since(since, Some(&remote_id))
        };
        let local_cursor = state.current_cursor();
        let applied = state.with_inbound_origin(&remote_id, |state| {
            options.applier.apply_inbound(state, entries)
        })?;
        state.set_peer_cursor(&remote_id, remote_cursor);
        Ok((outbound, local_cursor, outbound_full, applied))
    })?;
    stats.entries_applied = applied;
    stats.entries_sent = outbound.len();
    stats.full_snapshot |= outbound_full;
    options.check_cancel()?;

    let encrypted = encrypt_entries(app_key, outbound)?;
    write_message(
        reader.get_mut(),
        &Message::Delta {
            entries: encrypted,
            clock: local_cursor.clock,
            epoch: local_cursor.epoch,
            acked: remote_cursor_clock(state, &remote_id)?,
            acked_epoch: remote_cursor_epoch(state, &remote_id)?,
            full: outbound_full,
        },
    )?;
    if read_message(&mut reader)? != Message::Ack {
        return Err(Error::Protocol("expected ack".to_string()));
    }
    counters.fill(&mut stats);

    Ok(SyncOutcome {
        identity: remote_identity,
        fingerprint,
        stats,
    })
}

fn remote_cursor_clock<A: StateAccess>(state: &mut A, remote_id: &str) -> Result<u64> {
    state.with_state(|state| Ok(state.peer_cursor(remote_id).clock))
}

fn remote_cursor_epoch<A: StateAccess>(state: &mut A, remote_id: &str) -> Result<String> {
    state.with_state(|state| Ok(state.peer_cursor(remote_id).epoch))
}

pub fn link_with_device(
    identity: &Identity,
    device_keys: &DeviceKeys,
    app_key: &AppKey,
    device: SocketAddr,
    pairing_secret: Option<String>,
) -> Result<(Identity, String, AppKey)> {
    link_with_device_using(
        identity,
        device_keys,
        app_key,
        device,
        pairing_secret,
        &SyncOptions::default(),
    )
}

/// Links with a peer. The first link is trust-on-first-use: any certificate is
/// accepted and its fingerprint is returned for the caller to pin.
pub fn link_with_device_using(
    identity: &Identity,
    device_keys: &DeviceKeys,
    app_key: &AppKey,
    device: SocketAddr,
    pairing_secret: Option<String>,
    options: &SyncOptions,
) -> Result<(Identity, String, AppKey)> {
    options.check_cancel()?;
    let counters = ByteCounters::default();
    let mut reader = open_client(device, device_keys, options, &counters)?;

    write_message(
        reader.get_mut(),
        &Message::LinkRequest {
            identity: identity.clone(),
            app_key: Some(app_key.as_bytes().to_vec()),
            pairing_secret,
        },
    )?;

    let response = read_message(&mut reader)?;
    let fingerprint = device_fingerprint(reader.get_mut().conn.peer_certificates())?;
    let (remote_identity, accepted, remote_app_key) = match response {
        Message::LinkResponse {
            identity,
            accepted,
            app_key,
        } => (identity, accepted, app_key),
        _ => return Err(Error::Protocol("unexpected linking response".to_string())),
    };
    if let Some(cancel) = &options.cancel {
        cancel.release();
    }

    if !accepted {
        return Err(Error::Protocol(format!(
            "linking rejected by {}",
            remote_identity.device_id
        )));
    }

    let key_bytes = remote_app_key
        .ok_or_else(|| Error::Protocol("linking response missing app key".to_string()))?;
    let remote_app_key = AppKey::from_slice(&key_bytes)?;

    if remote_identity.app_id != identity.app_id {
        return Err(Error::Protocol(
            "app id mismatch during linking".to_string(),
        ));
    }

    Ok((remote_identity, fingerprint, remote_app_key))
}

fn handle_connection(stream: TcpStream, shared: &ListenerShared) -> Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(shared.options.io_timeout))?;
    stream.set_write_timeout(Some(shared.options.io_timeout))?;

    let identity = &shared.identity;
    let handler = shared.handler.as_ref();
    let app_key = handler.app_key()?;

    let stream = tls_server_stream(stream, &shared.device_keys)?;
    let mut reader = BufReader::new(stream);

    let message = match read_message(&mut reader) {
        Ok(message) => message,
        // A shutdown wake-up or a port scan closes the socket without a
        // handshake; that is not an error worth reporting.
        Err(Error::Io(error)) if is_disconnect(&error) => return Ok(()),
        Err(error) => return Err(error),
    };
    let fingerprint = device_fingerprint(reader.get_mut().conn.peer_certificates())?;

    match message {
        Message::LinkRequest {
            identity: device_identity,
            app_key: remote_app_key,
            pairing_secret,
        } => {
            if !device_identity.matches_app(handler.app_id()) {
                write_message(
                    reader.get_mut(),
                    &Message::LinkResponse {
                        identity: identity.clone(),
                        accepted: false,
                        app_key: None,
                    },
                )?;
                return Ok(());
            }
            if let Some(expected) = handler.pairing_secret() {
                let provided = pairing_secret.unwrap_or_default();
                if provided != expected {
                    write_message(
                        reader.get_mut(),
                        &Message::LinkResponse {
                            identity: identity.clone(),
                            accepted: false,
                            app_key: None,
                        },
                    )?;
                    return Ok(());
                }
            }
            let incoming_key = remote_app_key
                .ok_or_else(|| Error::Protocol("linking request missing app key".to_string()))?;
            let incoming_key = AppKey::from_slice(&incoming_key)?;
            let accepted = handler.approve_link_with_fingerprint(&device_identity, &fingerprint)?;
            if accepted && incoming_key != app_key {
                handler.set_app_key(&incoming_key)?;
            }
            let response_key = if accepted {
                Some(handler.app_key()?.as_bytes().to_vec())
            } else {
                None
            };
            write_message(
                reader.get_mut(),
                &Message::LinkResponse {
                    identity: identity.clone(),
                    accepted,
                    app_key: response_key,
                },
            )?;
        }
        Message::Hello {
            identity: device_identity,
            protocol_version,
        } => {
            if !device_identity.matches_app(handler.app_id()) {
                return Err(Error::Protocol("app id mismatch".to_string()));
            }
            if !handler.is_linked_with_fingerprint(&device_identity, &fingerprint) {
                if handler.is_linked(&device_identity) {
                    let device = DeviceInfo {
                        identity: device_identity.clone(),
                        address: reader.get_ref().sock.inner.peer_addr().ok(),
                        last_seen: Some(std::time::SystemTime::now()),
                        linked: true,
                        fingerprint: Some(fingerprint.clone()),
                    };
                    if let Some(sink) = &shared.options.event_sink {
                        sink.emit(Event::FingerprintChanged {
                            device,
                            fingerprint: fingerprint.clone(),
                        });
                    }
                    return Err(Error::FingerprintMismatch {
                        device_id: device_identity.device_id,
                        expected: "<pinned>".to_string(),
                        actual: fingerprint,
                    });
                }
                return Err(Error::Protocol("device not linked".to_string()));
            }
            write_message(reader.get_mut(), &Message::hello(identity.clone()))?;
            let remote_id = device_identity.device_id.clone();
            let applier = shared.options.applier.as_ref();

            let applied = match read_message(&mut reader)? {
                Message::SnapshotRequest => {
                    let snapshot = {
                        let mut state = lock_state(&shared.state)?;
                        applier.before_export(&mut state)?;
                        encrypt_entries(&app_key, state.snapshot())?
                    };
                    write_message(reader.get_mut(), &Message::Snapshot { entries: snapshot })?;
                    return Ok(());
                }
                Message::Snapshot { entries } => {
                    let entries = decrypt_entries(&app_key, entries)?;
                    let applied = {
                        let mut state = lock_state(&shared.state)?;
                        let applied = state.with_inbound_origin(&remote_id, |state| {
                            applier.apply_inbound(state, entries)
                        })?;
                        applier.after_apply(&state)?;
                        applied
                    };
                    write_message(reader.get_mut(), &Message::Ack)?;
                    applied
                }
                Message::SnapshotSince { clock, epoch } => {
                    let _ = protocol_version;
                    let (entries, local_cursor, acked, full) = {
                        let mut state = lock_state(&shared.state)?;
                        applier.before_export(&mut state)?;
                        let since = state.resolve_cursor(&PeerCursor { epoch, clock });
                        let full = state.is_full_snapshot_request(since);
                        let entries = if full {
                            state.snapshot()
                        } else {
                            state.delta_since(since, Some(&remote_id))
                        };
                        (
                            entries,
                            state.current_cursor(),
                            state.peer_cursor(&remote_id),
                            full,
                        )
                    };
                    let encrypted = encrypt_entries(&app_key, entries)?;
                    write_message(
                        reader.get_mut(),
                        &Message::Delta {
                            entries: encrypted,
                            clock: local_cursor.clock,
                            epoch: local_cursor.epoch,
                            acked: acked.clock,
                            acked_epoch: acked.epoch,
                            full,
                        },
                    )?;

                    let (entries, remote_cursor) = match read_message(&mut reader)? {
                        Message::Delta {
                            entries,
                            clock,
                            epoch,
                            ..
                        } => (entries, PeerCursor { epoch, clock }),
                        _ => return Err(Error::Protocol("expected delta".to_string())),
                    };
                    let entries = decrypt_entries(&app_key, entries)?;
                    let applied = {
                        let mut state = lock_state(&shared.state)?;
                        let applied = state.with_inbound_origin(&remote_id, |state| {
                            applier.apply_inbound(state, entries)
                        })?;
                        state.set_peer_cursor(&remote_id, remote_cursor);
                        applier.after_apply(&state)?;
                        applied
                    };
                    write_message(reader.get_mut(), &Message::Ack)?;
                    applied
                }
                _ => {
                    return Err(Error::Protocol(
                        "expected snapshot request, snapshot, or snapshot-since".to_string(),
                    ))
                }
            };

            if let Some(sink) = &shared.options.event_sink {
                sink.emit(Event::InboundSync {
                    device: DeviceInfo {
                        identity: device_identity,
                        address: reader.get_ref().sock.inner.peer_addr().ok(),
                        last_seen: Some(std::time::SystemTime::now()),
                        linked: true,
                        fingerprint: Some(fingerprint),
                    },
                    applied,
                });
            }
        }
        _ => {
            return Err(Error::Protocol(
                "expected hello or link request".to_string(),
            ))
        }
    }

    Ok(())
}

fn is_disconnect(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::BrokenPipe
    )
}

fn lock_state(state: &Arc<Mutex<State>>) -> Result<std::sync::MutexGuard<'_, State>> {
    state
        .lock()
        .map_err(|_| Error::Protocol("state lock poisoned".to_string()))
}

/// Legacy single-direction push kept for the version 1 fallback tests.
#[cfg(test)]
pub(crate) fn push_snapshot<F>(
    identity: &Identity,
    state: &State,
    device: SocketAddr,
    device_keys: &DeviceKeys,
    app_key: &AppKey,
    device_check: &F,
) -> Result<(Identity, String)>
where
    F: Fn(&Identity, &str) -> Result<()>,
{
    let counters = ByteCounters::default();
    let mut reader = open_client(device, device_keys, &SyncOptions::default(), &counters)?;
    write_message(reader.get_mut(), &Message::hello(identity.clone()))?;
    let (remote_identity, _) = read_hello(&mut reader, identity)?;
    let fingerprint = device_fingerprint(reader.get_mut().conn.peer_certificates())?;
    device_check(&remote_identity, &fingerprint)?;

    let snapshot = encrypt_entries(app_key, state.snapshot())?;
    write_message(reader.get_mut(), &Message::Snapshot { entries: snapshot })?;
    if read_message(&mut reader)? != Message::Ack {
        return Err(Error::Protocol("expected ack".to_string()));
    }
    Ok((remote_identity, fingerprint))
}

fn read_hello<R: std::io::BufRead>(reader: &mut R, identity: &Identity) -> Result<(Identity, u32)> {
    match read_message(reader)? {
        Message::Hello {
            identity: remote,
            protocol_version,
        } => {
            if remote.app_id != identity.app_id {
                return Err(Error::Protocol("app id mismatch".to_string()));
            }
            Ok((remote, protocol_version))
        }
        _ => Err(Error::Protocol("expected hello".to_string())),
    }
}

// ---------------------------------------------------------------------------
// Transport helpers
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct ByteCounters {
    sent: Arc<AtomicU64>,
    received: Arc<AtomicU64>,
}

impl ByteCounters {
    fn fill(&self, stats: &mut SyncStats) {
        stats.bytes_sent = self.sent.load(Ordering::SeqCst);
        stats.bytes_received = self.received.load(Ordering::SeqCst);
    }
}

/// A `TcpStream` that counts bytes crossing the socket (TLS bytes, i.e. what
/// is on the wire).
pub struct CountingStream {
    inner: TcpStream,
    sent: Arc<AtomicU64>,
    received: Arc<AtomicU64>,
}

impl Read for CountingStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.received.fetch_add(n as u64, Ordering::SeqCst);
        Ok(n)
    }
}

impl Write for CountingStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.sent.fetch_add(n as u64, Ordering::SeqCst);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

type ClientStream = StreamOwned<ClientConnection, CountingStream>;
type ServerStream = StreamOwned<ServerConnection, CountingStream>;

fn open_client(
    device: SocketAddr,
    device_keys: &DeviceKeys,
    options: &SyncOptions,
    counters: &ByteCounters,
) -> Result<BufReader<ClientStream>> {
    let stream = TcpStream::connect_timeout(&device, options.connect_timeout)?;
    stream.set_read_timeout(Some(options.io_timeout))?;
    stream.set_write_timeout(Some(options.io_timeout))?;
    if let Some(cancel) = &options.cancel {
        cancel.register(&stream)?;
    }
    let stream = CountingStream {
        inner: stream,
        sent: Arc::clone(&counters.sent),
        received: Arc::clone(&counters.received),
    };
    let stream = tls_client_stream_pinned(
        stream,
        device_keys,
        options.expected_fingerprint.as_deref(),
        options.expected_device_id.as_deref(),
    )?;
    Ok(BufReader::new(stream))
}

fn crypto_provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn tls_client_stream_pinned(
    stream: CountingStream,
    device_keys: &DeviceKeys,
    expected_fingerprint: Option<&str>,
    expected_device_id: Option<&str>,
) -> Result<ClientStream> {
    let verifier = Arc::new(PinnedServerVerifier::new(expected_fingerprint));
    let config = client_config(device_keys, verifier.clone())?;
    let server_name = server_name_for(expected_device_id);
    let mut conn = ClientConnection::new(config, server_name)?;
    let mut sock = stream;
    // Drive the handshake now so certificate pinning fails before any
    // application data is written.
    while conn.is_handshaking() {
        if let Err(error) = conn.complete_io(&mut sock) {
            if let Some(actual) = verifier.mismatch() {
                return Err(Error::FingerprintMismatch {
                    device_id: expected_device_id.unwrap_or("<unknown>").to_string(),
                    expected: expected_fingerprint.unwrap_or_default().to_string(),
                    actual,
                });
            }
            return Err(error.into());
        }
    }
    Ok(StreamOwned::new(conn, sock))
}

#[cfg(test)]
pub(crate) fn tls_client_stream(
    stream: TcpStream,
    device_keys: &DeviceKeys,
) -> Result<ClientStream> {
    let stream = CountingStream {
        inner: stream,
        sent: Arc::default(),
        received: Arc::default(),
    };
    tls_client_stream_pinned(stream, device_keys, None, None)
}

fn tls_server_stream(stream: TcpStream, device_keys: &DeviceKeys) -> Result<ServerStream> {
    let config = server_config(device_keys)?;
    let conn = ServerConnection::new(config)?;
    let stream = CountingStream {
        inner: stream,
        sent: Arc::default(),
        received: Arc::default(),
    };
    Ok(StreamOwned::new(conn, stream))
}

fn server_name_for(device_id: Option<&str>) -> ServerName<'static> {
    device_id
        .and_then(|id| ServerName::try_from(id.to_string()).ok())
        .unwrap_or_else(|| ServerName::try_from(DEFAULT_SNI).expect("default server name is valid"))
}

fn client_config(
    device_keys: &DeviceKeys,
    verifier: Arc<PinnedServerVerifier>,
) -> Result<Arc<ClientConfig>> {
    let certs = vec![CertificateDer::from(device_keys.cert_der().to_vec())];
    let key = PrivateKeyDer::Pkcs8(device_keys.key_der().to_vec().into());
    let config = ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(certs, key)?;
    Ok(Arc::new(config))
}

fn server_config(device_keys: &DeviceKeys) -> Result<Arc<ServerConfig>> {
    let certs = vec![CertificateDer::from(device_keys.cert_der().to_vec())];
    let key = PrivateKeyDer::Pkcs8(device_keys.key_der().to_vec().into());
    let verifier = Arc::new(TofuClientVerifier::new());
    let config = ServerConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()?
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs, key)?;
    Ok(Arc::new(config))
}

fn device_fingerprint(certs: Option<&[CertificateDer<'_>]>) -> Result<String> {
    let cert = certs
        .and_then(|certs| certs.first())
        .ok_or_else(|| Error::Protocol("missing device certificate".to_string()))?;
    Ok(crate::keys::fingerprint_cert(cert.as_ref()))
}

/// Server certificate verifier for device-to-device TLS.
///
/// Devices use self-signed certificates, so there is no chain to validate.
/// Without a pin the verifier accepts any certificate (trust on first use);
/// the caller then binds the peer's identity to the presented fingerprint.
/// With a pin, only the pinned leaf certificate is accepted.
#[derive(Debug)]
struct PinnedServerVerifier {
    expected: Option<String>,
    mismatch: Mutex<Option<String>>,
    provider: Arc<CryptoProvider>,
}

impl PinnedServerVerifier {
    fn new(expected: Option<&str>) -> Self {
        Self {
            expected: expected.map(|value| value.to_string()),
            mismatch: Mutex::new(None),
            provider: crypto_provider(),
        }
    }

    fn mismatch(&self) -> Option<String> {
        self.mismatch.lock().ok().and_then(|guard| guard.clone())
    }
}

impl ServerCertVerifier for PinnedServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        if let Some(expected) = &self.expected {
            let actual = crate::keys::fingerprint_cert(end_entity.as_ref());
            if *expected != actual {
                if let Ok(mut guard) = self.mismatch.lock() {
                    *guard = Some(actual);
                }
                return Err(rustls::Error::InvalidCertificate(
                    rustls::CertificateError::ApplicationVerificationFailure,
                ));
            }
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Client certificate verifier: requires a certificate and proof of key
/// possession, but accepts any issuer. The listener binds the client's claimed
/// identity to the presented fingerprint after the `Hello` and rejects linked
/// devices whose fingerprint changed.
#[derive(Debug)]
struct TofuClientVerifier {
    provider: Arc<CryptoProvider>,
}

impl TofuClientVerifier {
    fn new() -> Self {
        Self {
            provider: crypto_provider(),
        }
    }
}

impl ClientCertVerifier for TofuClientVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use super::{
        push_snapshot, tls_client_stream, tls_server_stream, CancelToken, ListenerOptions,
        SyncOptions,
    };
    use crate::{
        event_channel, read_message, sync_with_device, sync_with_device_using, write_message,
        AppKey, DeviceHandler, Event, Identity, Message, Result, State, SyncListener,
    };

    struct AllowAllHandler {
        app_id: String,
        keys: crate::DeviceKeys,
        app_key: AppKey,
    }

    impl DeviceHandler for AllowAllHandler {
        fn app_id(&self) -> &str {
            &self.app_id
        }

        fn is_linked(&self, _identity: &Identity) -> bool {
            true
        }

        fn approve_link(&self, _identity: &Identity) -> Result<bool> {
            Ok(true)
        }

        fn device_keys(&self) -> Result<crate::DeviceKeys> {
            Ok(self.keys.clone())
        }

        fn app_key(&self) -> Result<AppKey> {
            Ok(self.app_key.clone())
        }
    }

    struct RecordingHandler {
        app_id: String,
        linked: Mutex<HashSet<String>>,
        approve: bool,
        keys: crate::DeviceKeys,
        fingerprints: Mutex<HashMap<String, String>>,
        app_key: Mutex<AppKey>,
    }

    impl RecordingHandler {
        fn new(app_id: &str, approve: bool, keys: crate::DeviceKeys) -> Self {
            let app_key = AppKey::generate().expect("app key");
            Self {
                app_id: app_id.to_string(),
                linked: Mutex::new(HashSet::new()),
                approve,
                keys,
                fingerprints: Mutex::new(HashMap::new()),
                app_key: Mutex::new(app_key),
            }
        }

        fn pin(&self, device_id: &str, fingerprint: &str) {
            self.linked
                .lock()
                .expect("linked lock")
                .insert(device_id.to_string());
            self.fingerprints
                .lock()
                .expect("fingerprints lock")
                .insert(device_id.to_string(), fingerprint.to_string());
        }
    }

    impl DeviceHandler for RecordingHandler {
        fn app_id(&self) -> &str {
            &self.app_id
        }

        fn is_linked(&self, identity: &Identity) -> bool {
            self.linked
                .lock()
                .expect("linked lock")
                .contains(&identity.device_id)
        }

        fn approve_link(&self, identity: &Identity) -> Result<bool> {
            if self.approve {
                self.linked
                    .lock()
                    .expect("linked lock")
                    .insert(identity.device_id.clone());
                Ok(true)
            } else {
                Ok(false)
            }
        }

        fn device_keys(&self) -> Result<crate::DeviceKeys> {
            Ok(self.keys.clone())
        }

        fn app_key(&self) -> Result<AppKey> {
            Ok(self.app_key.lock().expect("app key lock").clone())
        }

        fn set_app_key(&self, app_key: &AppKey) -> Result<()> {
            *self.app_key.lock().expect("app key lock") = app_key.clone();
            Ok(())
        }

        fn is_linked_with_fingerprint(&self, identity: &Identity, fingerprint: &str) -> bool {
            if !self.is_linked(identity) {
                return false;
            }
            self.fingerprints
                .lock()
                .expect("fingerprints lock")
                .get(&identity.device_id)
                .map(|known| known == fingerprint)
                .unwrap_or(false)
        }

        fn approve_link_with_fingerprint(
            &self,
            identity: &Identity,
            fingerprint: &str,
        ) -> Result<bool> {
            if self.approve {
                self.pin(&identity.device_id, fingerprint);
                Ok(true)
            } else {
                Ok(false)
            }
        }
    }

    fn start_listener(
        state: Arc<Mutex<State>>,
        handler: Arc<dyn DeviceHandler>,
        options: ListenerOptions,
    ) -> SyncListener {
        let identity = Identity::new("listener", "com.example.app", "listener-user");
        SyncListener::start_with_options(
            "127.0.0.1:0".parse().expect("addr"),
            identity,
            state,
            handler,
            options,
        )
        .expect("listener start")
    }

    #[test]
    fn sync_round_trip_merges_entries() {
        let identity = Identity::new("listener", "com.example.app", "listener-user");
        let listener_keys = crate::DeviceKeys::generate(&identity).expect("listener keys");
        let app_key = AppKey::generate().expect("app key");
        let listener_state = Arc::new(Mutex::new(State::new("listener")));
        {
            let mut state = listener_state.lock().expect("state");
            state.set("alpha", b"one".to_vec());
        }

        let handler = Arc::new(AllowAllHandler {
            app_id: "com.example.app".to_string(),
            keys: listener_keys.clone(),
            app_key: app_key.clone(),
        });
        let listener = SyncListener::start(
            "127.0.0.1:0".parse().expect("addr"),
            identity,
            listener_state.clone(),
            handler,
        )
        .expect("listener start");

        let device_identity = Identity::new("device", "com.example.app", "device-user");
        let device_keys = crate::DeviceKeys::generate(&device_identity).expect("device keys");
        let device_app_key = app_key.clone();
        let mut device_state = State::new("device");
        device_state.set("beta", b"two".to_vec());

        sync_with_device(
            &device_identity,
            &mut device_state,
            listener.addr(),
            &device_keys,
            &device_app_key,
            |_, _| Ok(()),
        )
        .expect("sync");

        assert_eq!(device_state.get("alpha"), Some("one".as_bytes()));
        assert_eq!(device_state.get("beta"), Some("two".as_bytes()));

        let listener_snapshot = listener_state.lock().expect("state").snapshot();
        assert!(listener_snapshot.iter().any(|entry| entry.key == "beta"));

        listener.shutdown().expect("shutdown");
    }

    #[test]
    fn second_sync_sends_only_the_delta() {
        let identity = Identity::new("listener", "com.example.app", "listener-user");
        let listener_keys = crate::DeviceKeys::generate(&identity).expect("listener keys");
        let app_key = AppKey::generate().expect("app key");
        let listener_state = Arc::new(Mutex::new(State::new("listener")));
        {
            let mut state = listener_state.lock().expect("state");
            for index in 0..200 {
                state.set(format!("bulk-{index}"), vec![b'x'; 512]);
            }
        }
        let handler = Arc::new(AllowAllHandler {
            app_id: "com.example.app".to_string(),
            keys: listener_keys,
            app_key: app_key.clone(),
        });
        let listener = SyncListener::start(
            "127.0.0.1:0".parse().expect("addr"),
            identity,
            listener_state.clone(),
            handler,
        )
        .expect("listener start");

        let device_identity = Identity::new("device", "com.example.app", "device-user");
        let device_keys = crate::DeviceKeys::generate(&device_identity).expect("device keys");
        let mut device_state = State::new("device");
        for index in 0..200 {
            device_state.set(format!("local-{index}"), vec![b'y'; 512]);
        }
        let options = SyncOptions::default();

        let first = sync_with_device_using(
            &device_identity,
            &mut device_state,
            listener.addr(),
            &device_keys,
            &app_key,
            |_, _| Ok(()),
            &options,
        )
        .expect("first sync");
        assert!(first.stats.full_snapshot);
        assert_eq!(first.stats.entries_received, 200);
        assert_eq!(first.stats.entries_sent, 200);
        assert!(first.stats.bytes_received > 100_000);

        // Nothing changed: the exchange is empty in both directions.
        let idle = sync_with_device_using(
            &device_identity,
            &mut device_state,
            listener.addr(),
            &device_keys,
            &app_key,
            |_, _| Ok(()),
            &options,
        )
        .expect("idle sync");
        assert!(!idle.stats.full_snapshot);
        assert_eq!(idle.stats.entries_received, 0);
        assert_eq!(idle.stats.entries_sent, 0);

        // One small edit on each side: bytes are proportional to the edit.
        device_state.set("local-edit", b"small change".to_vec());
        listener_state
            .lock()
            .expect("state")
            .set("remote-edit", b"tiny".to_vec());
        let third = sync_with_device_using(
            &device_identity,
            &mut device_state,
            listener.addr(),
            &device_keys,
            &app_key,
            |_, _| Ok(()),
            &options,
        )
        .expect("delta sync");
        assert!(!third.stats.full_snapshot);
        assert_eq!(third.stats.entries_sent, 1);
        assert_eq!(third.stats.entries_received, 1);
        assert_eq!(third.stats.entries_applied, 1);
        assert!(
            third.stats.bytes_sent < 4_096,
            "sent {} bytes",
            third.stats.bytes_sent
        );
        assert!(
            third.stats.bytes_received < 4_096,
            "received {} bytes",
            third.stats.bytes_received
        );
        assert!(third.stats.bytes_received * 20 < first.stats.bytes_received);
        assert_eq!(device_state.get("remote-edit"), Some("tiny".as_bytes()));
        assert_eq!(
            listener_state.lock().expect("state").get("local-edit"),
            Some("small change".as_bytes())
        );

        listener.shutdown().expect("shutdown");
    }

    #[test]
    fn peer_reset_falls_back_to_full_snapshot() {
        let identity = Identity::new("listener", "com.example.app", "listener-user");
        let listener_keys = crate::DeviceKeys::generate(&identity).expect("listener keys");
        let app_key = AppKey::generate().expect("app key");
        let listener_state = Arc::new(Mutex::new(State::new("listener")));
        listener_state
            .lock()
            .expect("state")
            .set("alpha", b"one".to_vec());
        let handler = Arc::new(AllowAllHandler {
            app_id: "com.example.app".to_string(),
            keys: listener_keys,
            app_key: app_key.clone(),
        });
        let listener = SyncListener::start(
            "127.0.0.1:0".parse().expect("addr"),
            identity,
            listener_state.clone(),
            handler,
        )
        .expect("listener start");

        let device_identity = Identity::new("device", "com.example.app", "device-user");
        let device_keys = crate::DeviceKeys::generate(&device_identity).expect("device keys");
        let mut device_state = State::new("device");
        let options = SyncOptions::default();
        let sync = |state: &mut State| {
            sync_with_device_using(
                &device_identity,
                state,
                listener.addr(),
                &device_keys,
                &app_key,
                |_, _| Ok(()),
                &options,
            )
            .expect("sync")
        };
        sync(&mut device_state);
        assert!(!sync(&mut device_state).stats.full_snapshot);

        // The listener loses its state (new epoch). The device still holds a
        // cursor into the old history and must receive a full snapshot again.
        {
            let mut state = listener_state.lock().expect("state");
            *state = State::new("listener");
            state.set("fresh", b"new".to_vec());
        }
        let outcome = sync(&mut device_state);
        assert!(outcome.stats.full_snapshot);
        assert_eq!(device_state.get("fresh"), Some("new".as_bytes()));
        assert_eq!(
            listener_state.lock().expect("state").get("alpha"),
            Some("one".as_bytes())
        );

        listener.shutdown().expect("shutdown");
    }

    #[test]
    fn sync_with_device_errors_on_unexpected_message() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let listener_identity = Identity::new("listener", "com.example.app", "user");
        let listener_keys = crate::DeviceKeys::generate(&listener_identity).expect("listener keys");
        let listener_app_key = AppKey::generate().expect("app key");

        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let stream = tls_server_stream(stream, &listener_keys).expect("tls server");
            let mut reader = std::io::BufReader::new(stream);

            let _ = read_message(&mut reader).expect("hello");
            write_message(reader.get_mut(), &Message::hello(listener_identity)).expect("hello");
            let _ = read_message(&mut reader).expect("snapshot since");
            write_message(reader.get_mut(), &Message::Ack).expect("write ack");
        });

        let mut state = State::new("device");
        let identity = Identity::new("device", "com.example.app", "user");
        let device_keys = crate::DeviceKeys::generate(&identity).expect("device keys");
        let result = sync_with_device(
            &identity,
            &mut state,
            addr,
            &device_keys,
            &listener_app_key,
            |_, _| Ok(()),
        );
        assert!(result.is_err());

        handle.join().expect("join");
    }

    #[test]
    fn legacy_peer_uses_two_connection_snapshot_exchange() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let listener_identity = Identity::new("listener", "com.example.app", "user");
        let listener_keys = crate::DeviceKeys::generate(&listener_identity).expect("listener keys");
        let app_key = AppKey::generate().expect("app key");
        let server_key = app_key.clone();

        let handle = thread::spawn(move || {
            let legacy_hello = Message::Hello {
                identity: listener_identity.clone(),
                protocol_version: 1,
            };
            // Push connection.
            let (stream, _) = listener.accept().expect("accept push");
            let stream = tls_server_stream(stream, &listener_keys).expect("tls server");
            let mut reader = std::io::BufReader::new(stream);
            let _ = read_message(&mut reader).expect("hello");
            write_message(reader.get_mut(), &legacy_hello).expect("hello response");
            let pushed = match read_message(&mut reader).expect("snapshot") {
                Message::Snapshot { entries } => entries,
                other => panic!("unexpected {other:?}"),
            };
            write_message(reader.get_mut(), &Message::Ack).expect("write ack");

            // Pull connection.
            let (stream, _) = listener.accept().expect("accept pull");
            let stream = tls_server_stream(stream, &listener_keys).expect("tls server");
            let mut reader = std::io::BufReader::new(stream);
            let _ = read_message(&mut reader).expect("hello");
            write_message(reader.get_mut(), &legacy_hello).expect("hello response");
            assert_eq!(
                read_message(&mut reader).expect("request"),
                Message::SnapshotRequest
            );
            let mut state = State::new("listener");
            state.set("from-legacy", b"old".to_vec());
            let entries = crate::encrypt_entries(&server_key, state.snapshot()).expect("encrypt");
            write_message(reader.get_mut(), &Message::Snapshot { entries }).expect("snapshot");
            pushed.len()
        });

        let mut state = State::new("device");
        state.set("local", b"value".to_vec());
        let identity = Identity::new("device", "com.example.app", "user");
        let device_keys = crate::DeviceKeys::generate(&identity).expect("device keys");
        let outcome = sync_with_device_using(
            &identity,
            &mut state,
            addr,
            &device_keys,
            &app_key,
            |_, _| Ok(()),
            &SyncOptions::default(),
        )
        .expect("legacy sync");
        assert_eq!(outcome.stats.protocol_version, 1);
        assert!(outcome.stats.full_snapshot);
        assert_eq!(state.get("from-legacy"), Some("old".as_bytes()));
        assert_eq!(handle.join().expect("join"), 1);
    }

    #[test]
    fn listener_still_answers_legacy_push() {
        let identity = Identity::new("listener", "com.example.app", "listener-user");
        let listener_keys = crate::DeviceKeys::generate(&identity).expect("listener keys");
        let app_key = AppKey::generate().expect("app key");
        let listener_state = Arc::new(Mutex::new(State::new("listener")));
        let handler = Arc::new(AllowAllHandler {
            app_id: "com.example.app".to_string(),
            keys: listener_keys,
            app_key: app_key.clone(),
        });
        let listener = SyncListener::start(
            "127.0.0.1:0".parse().expect("addr"),
            identity,
            listener_state.clone(),
            handler,
        )
        .expect("listener start");

        let device_identity = Identity::new("device", "com.example.app", "device-user");
        let device_keys = crate::DeviceKeys::generate(&device_identity).expect("device keys");
        let mut device_state = State::new("device");
        device_state.set("legacy", b"push".to_vec());
        push_snapshot(
            &device_identity,
            &device_state,
            listener.addr(),
            &device_keys,
            &app_key,
            &|_, _| Ok(()),
        )
        .expect("push");
        assert_eq!(
            listener_state.lock().expect("state").get("legacy"),
            Some("push".as_bytes())
        );
        listener.shutdown().expect("shutdown");
    }

    #[test]
    fn link_request_updates_allowlist() {
        let identity = Identity::new("listener", "com.example.app", "listener-user");
        let listener_keys = crate::DeviceKeys::generate(&identity).expect("listener keys");
        let handler = Arc::new(RecordingHandler::new(
            "com.example.app",
            true,
            listener_keys,
        ));
        let listener_state = Arc::new(Mutex::new(State::new("listener")));
        let listener = SyncListener::start(
            "127.0.0.1:0".parse().expect("addr"),
            identity,
            listener_state,
            handler.clone(),
        )
        .expect("listener start");

        let device_identity = Identity::new("device", "com.example.app", "device-user");
        let device_keys = crate::DeviceKeys::generate(&device_identity).expect("device keys");
        let app_key = handler.app_key().expect("app key");
        let stream = TcpStream::connect(listener.addr()).expect("connect");
        let stream = tls_client_stream(stream, &device_keys).expect("tls client");
        let mut reader = std::io::BufReader::new(stream);

        write_message(
            reader.get_mut(),
            &Message::LinkRequest {
                identity: device_identity.clone(),
                app_key: Some(app_key.as_bytes().to_vec()),
                pairing_secret: None,
            },
        )
        .expect("link request");

        let response = read_message(&mut reader).expect("link response");
        match response {
            Message::LinkResponse { accepted, .. } => assert!(accepted),
            _ => panic!("expected link response"),
        }

        assert!(handler.is_linked_with_fingerprint(&device_identity, device_keys.fingerprint()));
        listener.shutdown().expect("shutdown");
    }

    #[test]
    fn sync_rejects_unlinked_device() {
        let identity = Identity::new("listener", "com.example.app", "listener-user");
        let listener_keys = crate::DeviceKeys::generate(&identity).expect("listener keys");
        let app_key = AppKey::generate().expect("app key");
        let handler = Arc::new(RecordingHandler::new(
            "com.example.app",
            false,
            listener_keys,
        ));
        let listener_state = Arc::new(Mutex::new(State::new("listener")));
        let listener = SyncListener::start(
            "127.0.0.1:0".parse().expect("addr"),
            identity,
            listener_state,
            handler,
        )
        .expect("listener start");

        let device_identity = Identity::new("device", "com.example.app", "user");
        let mut state = State::new("device");
        let device_keys = crate::DeviceKeys::generate(&device_identity).expect("device keys");
        let result = sync_with_device(
            &device_identity,
            &mut state,
            listener.addr(),
            &device_keys,
            &app_key,
            |_, _| Ok(()),
        );
        assert!(result.is_err());

        listener.shutdown().expect("shutdown");
    }

    #[test]
    fn pinned_fingerprint_rejects_other_certificate_at_handshake() {
        let identity = Identity::new("listener", "com.example.app", "listener-user");
        let listener_keys = crate::DeviceKeys::generate(&identity).expect("listener keys");
        let app_key = AppKey::generate().expect("app key");
        let handler = Arc::new(AllowAllHandler {
            app_id: "com.example.app".to_string(),
            keys: listener_keys.clone(),
            app_key: app_key.clone(),
        });
        let listener_state = Arc::new(Mutex::new(State::new("listener")));
        let listener = SyncListener::start(
            "127.0.0.1:0".parse().expect("addr"),
            identity.clone(),
            listener_state,
            handler,
        )
        .expect("listener start");

        let device_identity = Identity::new("device", "com.example.app", "user");
        let device_keys = crate::DeviceKeys::generate(&device_identity).expect("device keys");
        let mut state = State::new("device");

        // Correct pin: works.
        let pinned = SyncOptions::default()
            .with_expected_fingerprint(listener_keys.fingerprint())
            .with_expected_device_id("listener");
        sync_with_device_using(
            &device_identity,
            &mut state,
            listener.addr(),
            &device_keys,
            &app_key,
            |_, _| Ok(()),
            &pinned,
        )
        .expect("pinned sync");

        // Wrong pin (the listener rotated its keys): the handshake fails and
        // the error names both fingerprints.
        let rotated = crate::DeviceKeys::generate(&identity).expect("rotated keys");
        let wrong = SyncOptions::default()
            .with_expected_fingerprint(rotated.fingerprint())
            .with_expected_device_id("listener");
        let error = sync_with_device_using(
            &device_identity,
            &mut state,
            listener.addr(),
            &device_keys,
            &app_key,
            |_, _| Ok(()),
            &wrong,
        )
        .expect_err("mismatch");
        match error {
            crate::Error::FingerprintMismatch {
                device_id,
                expected,
                actual,
            } => {
                assert_eq!(device_id, "listener");
                assert_eq!(expected, rotated.fingerprint());
                assert_eq!(actual, listener_keys.fingerprint());
            }
            other => panic!("unexpected error {other}"),
        }

        listener.shutdown().expect("shutdown");
    }

    #[test]
    fn listener_reports_fingerprint_change_for_linked_device() {
        let identity = Identity::new("listener", "com.example.app", "listener-user");
        let listener_keys = crate::DeviceKeys::generate(&identity).expect("listener keys");
        let handler = Arc::new(RecordingHandler::new(
            "com.example.app",
            true,
            listener_keys,
        ));
        let app_key = handler.app_key().expect("app key");
        let device_identity = Identity::new("device", "com.example.app", "user");
        let old_keys = crate::DeviceKeys::generate(&device_identity).expect("old keys");
        handler.pin("device", old_keys.fingerprint());

        let (sink, events) = event_channel();
        let listener = start_listener(
            Arc::new(Mutex::new(State::new("listener"))),
            handler,
            ListenerOptions::default().with_event_sink(sink),
        );

        let new_keys = crate::DeviceKeys::generate(&device_identity).expect("new keys");
        let mut state = State::new("device");
        let result = sync_with_device(
            &device_identity,
            &mut state,
            listener.addr(),
            &new_keys,
            &app_key,
            |_, _| Ok(()),
        );
        assert!(result.is_err());

        let mut saw_change = false;
        while let Ok(event) = events.recv_timeout(Duration::from_secs(2)) {
            if let Event::FingerprintChanged {
                device,
                fingerprint,
            } = event
            {
                assert_eq!(device.identity.device_id, "device");
                assert_eq!(fingerprint, new_keys.fingerprint());
                saw_change = true;
                break;
            }
        }
        assert!(saw_change, "expected a FingerprintChanged event");
        listener.shutdown().expect("shutdown");
    }

    #[test]
    fn cancelled_token_aborts_before_connecting() {
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(cancel.is_cancelled());
        let identity = Identity::new("device", "com.example.app", "user");
        let device_keys = crate::DeviceKeys::generate(&identity).expect("keys");
        let app_key = AppKey::generate().expect("app key");
        let mut state = State::new("device");
        let options = SyncOptions::default().with_cancel(cancel.clone());
        let error = sync_with_device_using(
            &identity,
            &mut state,
            "127.0.0.1:1".parse().expect("addr"),
            &device_keys,
            &app_key,
            |_, _| Ok(()),
            &options,
        )
        .expect_err("cancelled");
        assert!(matches!(error, crate::Error::Cancelled));
        cancel.reset();
        assert!(!cancel.is_cancelled());
    }

    #[test]
    fn cancel_unblocks_a_stalled_peer() {
        // A peer that accepts and never answers would hold the caller for the
        // io timeout; cancelling shuts the socket down immediately.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let stall = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            thread::sleep(Duration::from_secs(3));
            drop(stream);
        });

        let identity = Identity::new("device", "com.example.app", "user");
        let device_keys = crate::DeviceKeys::generate(&identity).expect("keys");
        let app_key = AppKey::generate().expect("app key");
        let cancel = CancelToken::new();
        let canceller = cancel.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(200));
            canceller.cancel();
        });
        let started = std::time::Instant::now();
        let options = SyncOptions::default()
            .with_cancel(cancel)
            .with_timeouts(Duration::from_secs(5), Duration::from_secs(10));
        let mut state = State::new("device");
        let error = sync_with_device_using(
            &identity,
            &mut state,
            addr,
            &device_keys,
            &app_key,
            |_, _| Ok(()),
            &options,
        )
        .expect_err("cancelled");
        assert!(matches!(error, crate::Error::Cancelled), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
        stall.join().expect("join");
    }

    #[test]
    fn listener_shutdown_wakes_blocking_accept_on_unspecified_bind() {
        let identity = Identity::new("listener", "com.example.app", "listener-user");
        let listener_keys = crate::DeviceKeys::generate(&identity).expect("listener keys");
        let handler = Arc::new(AllowAllHandler {
            app_id: "com.example.app".to_string(),
            keys: listener_keys,
            app_key: AppKey::generate().expect("app key"),
        });
        let listener = SyncListener::start(
            "0.0.0.0:0".parse().expect("addr"),
            identity,
            Arc::new(Mutex::new(State::new("listener"))),
            handler,
        )
        .expect("listener start");
        let started = std::time::Instant::now();
        listener.shutdown().expect("shutdown");
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
