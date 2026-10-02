use super::pairing::Handler;
use super::*;
use crate::{read_message, write_message, CancelToken, Message, SyncOptions};
use std::{
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
pub(super) struct Runtime {
    pub address: SocketAddr,
    pub cancel: CancelToken,
    workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    accept: JoinHandle<()>,
    scheduler: JoinHandle<()>,
    discovery: JoinHandle<()>,
    advertiser: Option<crate::MdnsAdvertiser>,
}
pub(super) fn addresses(addr: SocketAddr) -> Result<Vec<SocketAddr>> {
    if !addr.ip().is_unspecified() {
        return Ok(vec![addr]);
    }
    let mut addresses: Vec<_> = if_addrs::get_if_addrs()?
        .into_iter()
        .filter(|i| !i.is_loopback())
        .map(|i| SocketAddr::new(i.ip(), addr.port()))
        .collect();
    addresses.push(SocketAddr::from(([127, 0, 0, 1], addr.port())));
    Ok(addresses)
}
impl Session {
    pub fn start(&self) -> Result<SocketAddr> {
        let _lifecycle = lock(&self.inner.lifecycle)?;
        if let Some(r) = lock(&self.inner.runtime)?.as_ref() {
            return Ok(r.address);
        }
        let listener = TcpListener::bind(self.inner.config.listen)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        self.inner.mutate(|e| {
            e.generation = e
                .generation
                .checked_add(1)
                .ok_or_else(|| Error::Protocol("generation exhausted".into()))?;
            e.phase = SessionPhase::Running;
            Ok(())
        })?;
        let identity = lock(&self.inner.envelope)?.identity.clone();
        let advertiser = if self.inner.config.advertise {
            match crate::register_mdns_metadata(&identity, address, &self.inner.config.metadata) {
                Ok(a) => Some(a),
                Err(e) => {
                    self.inner.diagnostic(None, &e);
                    None
                }
            }
        } else {
            None
        };
        let browser = advertiser
            .as_ref()
            .map(|a| a.browse_managed())
            .transpose()?;
        let cancel = CancelToken::new();
        let workers = Arc::new(Mutex::new(Vec::<JoinHandle<()>>::new()));
        let inner = self.inner.clone();
        let accept_cancel = cancel.clone();
        let accept_workers = workers.clone();
        let accept = thread::spawn(move || {
            while !accept_cancel.is_cancelled() {
                match listener.accept() {
                    Ok((socket, _)) => {
                        if accept_cancel.is_cancelled() {
                            let _ = socket.shutdown(std::net::Shutdown::Both);
                            break;
                        }
                        let session = inner.clone();
                        let token = accept_cancel.clone();
                        if let Ok(mut workers) = accept_workers.lock() {
                            let mut pending = Vec::new();
                            for worker in workers.drain(..) {
                                if worker.is_finished() {
                                    let _ = worker.join();
                                } else {
                                    pending.push(worker);
                                }
                            }
                            *workers = pending;
                            if workers.len() >= 32 {
                                let _ = socket.shutdown(std::net::Shutdown::Both);
                                continue;
                            }
                            workers.push(thread::spawn(move || {
                                if let Err(e) = inbound(socket, &session, &token) {
                                    if !token.is_cancelled() {
                                        session.diagnostic(None, &e);
                                    }
                                }
                            }));
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(e) => {
                        inner.diagnostic(None, &e.into());
                        break;
                    }
                }
            }
        });
        let inner = self.inner.clone();
        let schedule_cancel = cancel.clone();
        let scheduler = thread::spawn(move || {
            let mut retries: BTreeMap<String, (u32, Instant)> = BTreeMap::new();
            let mut recovery_at = Instant::now();
            while !schedule_cancel.is_cancelled() {
                if Instant::now() >= recovery_at {
                    if let Err(error) = super::pairing::recover_pending(&inner, &schedule_cancel) {
                        if !schedule_cancel.is_cancelled() {
                            inner.diagnostic(None, &error);
                        }
                    }
                    recovery_at = Instant::now() + Duration::from_secs(2);
                }
                let peers = match lock(&inner.envelope) {
                    Ok(e) => e
                        .peers
                        .values()
                        .filter(|p| {
                            !p.revoked
                                && p.state != PeerState::Paused
                                && p.state != PeerState::NeedsRepair
                        })
                        .cloned()
                        .collect::<Vec<_>>(),
                    Err(e) => {
                        inner.diagnostic(None, &e);
                        break;
                    }
                };
                retries.retain(|id, _| peers.iter().any(|p| &p.identity.device_id == id));
                for peer in peers {
                    if schedule_cancel.is_cancelled() {
                        break;
                    }
                    if retries
                        .get(&peer.identity.device_id)
                        .is_some_and(|(_, next)| *next > Instant::now())
                    {
                        continue;
                    }
                    match outbound(&inner, &peer, &schedule_cancel, address) {
                        Ok(()) => {
                            retries.remove(&peer.identity.device_id);
                        }
                        Err(e) => {
                            if schedule_cancel.is_cancelled() {
                                break;
                            }
                            let changed = matches!(e, Error::FingerprintMismatch { .. });
                            let _ = inner.mutate(|j| {
                                if let Some(p) = j.peers.get_mut(&peer.identity.device_id) {
                                    if !p.revoked {
                                        p.state = if changed {
                                            PeerState::NeedsRepair
                                        } else {
                                            PeerState::Waiting
                                        };
                                    }
                                }
                                Ok(())
                            });
                            inner.diagnostic(Some(peer.identity.device_id.clone()), &e);
                            let attempt = retries
                                .get(&peer.identity.device_id)
                                .map(|(n, _)| n + 1)
                                .unwrap_or(1)
                                .min(6);
                            retries.insert(
                                peer.identity.device_id,
                                (
                                    attempt,
                                    Instant::now() + Duration::from_millis(100 * (1 << attempt)),
                                ),
                            );
                        }
                    }
                }
                if schedule_cancel.is_cancelled() {
                    break;
                }
                let retry_wait = retries
                    .values()
                    .map(|(_, next)| next.saturating_duration_since(Instant::now()))
                    .min()
                    .unwrap_or(inner.config.catch_up);
                if inner
                    .wake
                    .1
                    .recv_timeout(inner.config.catch_up.min(retry_wait))
                    .is_ok()
                {
                    let deadline = Instant::now() + inner.config.debounce;
                    while Instant::now() < deadline && !schedule_cancel.is_cancelled() {
                        let _ = inner.wake.1.recv_timeout(
                            deadline
                                .saturating_duration_since(Instant::now())
                                .min(Duration::from_millis(20)),
                        );
                    }
                }
            }
        });
        let inner = self.inner.clone();
        let discovery_cancel = cancel.clone();
        let discovery = thread::spawn(move || {
            let mut fullnames = BTreeMap::<String, String>::new();
            while !discovery_cancel.is_cancelled() {
                if let Some(browser) = &browser {
                    match browser.recv_timeout(Duration::from_millis(200)) {
                        Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) => {
                            if let Some(peer) = crate::discovery::parse_peer_info(
                                &info,
                                &inner.config.metadata.manifest.app_id,
                            ) {
                                if peer.identity != identity {
                                    fullnames.insert(
                                        info.get_fullname().to_string(),
                                        peer.identity.device_id.clone(),
                                    );
                                    let changed = lock(&inner.discovered).is_ok_and(|mut d| {
                                        let changed =
                                            d.get(&peer.identity.device_id) != Some(&peer);
                                        d.insert(peer.identity.device_id.clone(), peer.clone());
                                        changed
                                    });
                                    if changed {
                                        inner.emit(SessionEvent::Changed);
                                    }
                                    match inner.mutate(|e| {
                                        let mut changed = false;
                                        if let Some(p) = e.peers.get_mut(&peer.identity.device_id) {
                                            if !p.revoked
                                                && p.identity == peer.identity
                                                && !peer.addresses.is_empty()
                                                && p.addresses != peer.addresses
                                            {
                                                p.addresses = peer.addresses.clone();
                                                changed = true;
                                            }
                                        }
                                        for pending in e.preparations.values_mut() {
                                            if pending.outcome.identity == peer.identity
                                                && !peer.addresses.is_empty()
                                                && pending.addresses != peer.addresses
                                            {
                                                pending.addresses = peer.addresses.clone();
                                                changed = true;
                                            }
                                        }
                                        Ok(changed)
                                    }) {
                                        Ok(true) => inner.wake(),
                                        Ok(false) => {}
                                        Err(e) => inner.diagnostic(None, &e),
                                    }
                                }
                            }
                        }
                        Ok(mdns_sd::ServiceEvent::ServiceRemoved(_, name)) => {
                            if let Some(id) = fullnames.remove(&name) {
                                if let Ok(mut peers) = inner.discovered.lock() {
                                    peers.remove(&id);
                                }
                                inner.emit(SessionEvent::Changed);
                            }
                        }
                        Ok(_) | Err(mdns_sd::RecvTimeoutError::Timeout) => {}
                        Err(e) => {
                            inner.diagnostic(
                                None,
                                &Error::Protocol(format!("discovery browser closed: {e}")),
                            );
                            break;
                        }
                    }
                    let expired = lock(&inner.invitation).is_ok_and(|d| {
                        d.as_ref()
                            .is_some_and(|d| !inner.pairing.is_open(&d.invitation_id))
                    });
                    if expired {
                        if let Ok(mut invitation) = inner.invitation.lock() {
                            *invitation = None;
                        }
                        if let Err(error) = inner.refresh_advertisement() {
                            inner.diagnostic(None, &error);
                        }
                    }
                } else {
                    thread::sleep(Duration::from_millis(20));
                }
            }
        });
        *lock(&self.inner.runtime)? = Some(Runtime {
            address,
            cancel,
            workers,
            accept,
            scheduler,
            discovery,
            advertiser,
        });
        Ok(address)
    }
    pub fn address(&self) -> Result<SocketAddr> {
        lock(&self.inner.runtime)?
            .as_ref()
            .map(|r| r.address)
            .ok_or_else(|| Error::Protocol("session is not running".into()))
    }
    fn stop(&self, phase: SessionPhase) -> Result<()> {
        let mut failures = Vec::new();
        // Poisoned coordination state must not strand owned threads or sockets.
        // Only cleanup recovers these guards; normal operations remain fail-closed.
        let _lifecycle = match self.inner.lifecycle.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                failures.push(Error::Protocol("lifecycle lock poisoned".into()));
                poisoned.into_inner()
            }
        };
        if let Err(error) = self.inner.mutate(|e| {
            if e.phase != phase {
                e.generation = e
                    .generation
                    .checked_add(1)
                    .ok_or_else(|| Error::Protocol("generation exhausted".into()))?;
                e.phase = phase;
            }
            Ok(())
        }) {
            failures.push(error);
        }
        if let Err(error) = self.inner.pairing.close() {
            failures.push(error);
        }
        let mut invitation = match self.inner.invitation.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                failures.push(Error::Protocol("invitation lock poisoned".into()));
                poisoned.into_inner()
            }
        };
        *invitation = None;
        drop(invitation);
        // Fence pre-auth registrations before scanning their independent socket tokens.
        {
            let runtime = match self.inner.runtime.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            if let Some(runtime) = runtime.as_ref() {
                runtime.cancel.cancel();
            }
        }
        let operations = match self.inner.operations.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                failures.push(Error::Protocol("operation registry poisoned".into()));
                poisoned.into_inner()
            }
        };
        for token in operations.values() {
            token.cancel();
        }
        drop(operations);
        let runtime = match self.inner.runtime.lock() {
            Ok(mut guard) => guard.take(),
            Err(poisoned) => {
                failures.push(Error::Protocol("runtime lock poisoned".into()));
                poisoned.into_inner().take()
            }
        };
        if let Some(runtime) = runtime {
            runtime.cancel.cancel();
            self.inner.wake();
            for (worker, name) in [
                (runtime.accept, "listener"),
                (runtime.scheduler, "scheduler"),
                (runtime.discovery, "discovery"),
            ] {
                if worker.join().is_err() {
                    failures.push(Error::Protocol(format!("{name} worker panicked")));
                }
            }
            let workers: Vec<_> = match runtime.workers.lock() {
                Ok(mut guard) => guard.drain(..).collect(),
                Err(poisoned) => {
                    failures.push(Error::Protocol("inbound registry poisoned".into()));
                    poisoned.into_inner().drain(..).collect()
                }
            };
            for worker in workers {
                if worker.join().is_err() {
                    failures.push(Error::Protocol("inbound worker panicked".into()));
                }
            }
            if let Some(advertiser) = runtime.advertiser {
                if let Err(error) = advertiser.shutdown() {
                    failures.push(error);
                }
            }
        }
        match failures.len() {
            0 => Ok(()),
            1 => Err(failures.remove(0)),
            _ => Err(Error::Protocol(format!(
                "shutdown cleanup errors: {}",
                failures
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            ))),
        }
    }
    pub fn pause(&self) -> Result<()> {
        self.stop(SessionPhase::Paused)
    }
    pub fn shutdown(&self) -> Result<()> {
        self.stop(SessionPhase::Stopped)
    }
    pub fn resume(&self) -> Result<SocketAddr> {
        self.start()
    }
}
fn context(inner: &Arc<Inner>) -> Result<Handler> {
    let e = lock(&inner.envelope)?;
    if e.phase != SessionPhase::Running {
        return Err(Error::Cancelled);
    }
    Ok(Handler {
        inner: inner.clone(),
        generation: e.generation,
        expected: None,
        addresses: Vec::new(),
        recovering: false,
        repair: false,
    })
}
fn check(e: &Envelope, handler: &Handler, id: &Identity, fp: &str) -> Result<()> {
    if e.phase != SessionPhase::Running || e.generation != handler.generation {
        return Err(Error::Cancelled);
    }
    let p = e
        .peers
        .get(&id.device_id)
        .ok_or_else(|| Error::Protocol("peer is not enrolled".into()))?;
    if p.revoked
        || p.state == PeerState::Paused
        || p.state == PeerState::NeedsRepair
        || p.identity != *id
        || p.fingerprint != fp
    {
        return fail_code(
            SessionErrorCode::PeerRevoked,
            "peer is revoked, paused or changed",
        );
    }
    Ok(())
}
fn hello(inner: &Inner, peer: &Peer, address: SocketAddr) -> Result<Message> {
    let e = lock(&inner.envelope)?;
    Ok(Message::ManagedHello {
        identity: e.identity.clone(),
        metadata: inner.config.metadata.clone(),
        cursor: peer.received.clone(),
        processed: peer.processed.clone(),
        addresses: addresses(address)?,
    })
}
fn authenticate(
    inner: &Inner,
    handler: &Handler,
    message: Message,
    fp: &str,
) -> Result<(Peer, ManagedReceipt)> {
    match message {
        Message::ManagedHello {
            identity,
            metadata,
            cursor,
            processed,
            addresses,
        } => {
            if addresses.len() > 32
                || !inner
                    .config
                    .metadata
                    .manifest
                    .compatible_with(&metadata.manifest)
            {
                return fail_code(
                    SessionErrorCode::IncompatibleSchema,
                    "incompatible managed schema",
                );
            }
            inner.mutate(|e| {
                check(e, handler, &identity, fp)?;
                let current = e.peers.get(&identity.device_id).ok_or(Error::Cancelled)?;
                // Unproven cursor hints can never suppress authoritative data.
                let cursor = if cursor == ManagedReceipt::default()
                    || receipt::verify(inner, e, current, &cursor).is_ok()
                {
                    cursor
                } else {
                    ManagedReceipt::default()
                };
                if processed != ManagedReceipt::default() {
                    if let Err(error) = receipt::verify(inner, e, current, &processed) {
                        inner.diagnostic(Some(identity.device_id.clone()), &error);
                    } else {
                        let p = e
                            .peers
                            .get_mut(&identity.device_id)
                            .ok_or(Error::Cancelled)?;
                        // Truthful application processing also proves durable storage,
                        // even when the original Stored response was lost.
                        if p.stored.epoch != processed.epoch
                            || processed.sequence > p.stored.sequence
                        {
                            p.stored = processed.clone();
                        }
                        if p.applied.epoch != processed.epoch
                            || processed.sequence > p.applied.sequence
                        {
                            p.applied = processed.clone();
                        }
                    }
                }
                let p = e
                    .peers
                    .get_mut(&identity.device_id)
                    .ok_or(Error::Cancelled)?;
                p.addresses = addresses;
                Ok((p.clone(), cursor))
            })
        }
        _ => fail("expected authenticated managed hello"),
    }
}
fn outbound(
    inner: &Arc<Inner>,
    peer: &Peer,
    cancel: &CancelToken,
    address: SocketAddr,
) -> Result<()> {
    let handler = context(inner)?;
    let mut last = Error::Protocol("peer destination not yet known".into());
    for target in &inner.endpoint_hints(&peer.identity, &peer.addresses)? {
        cancel.check()?;
        let operation = OperationGuard::new(inner)?;
        let token = operation.token.clone();
        let options = SyncOptions::new()
            .with_cancel(token.clone())
            .with_expected_fingerprint(&peer.fingerprint)
            .with_expected_device_id(&peer.identity.device_id)
            .with_timeouts(Duration::from_millis(300), Duration::from_secs(2));
        let counters = crate::sync::ByteCounters::default();
        match crate::sync::open_client(*target, &inner.keys, &options, &counters) {
            Ok(mut reader) => {
                cancel.check()?;
                reader
                    .get_mut()
                    .sock
                    .inner
                    .set_read_timeout(Some(inner.config.authenticated_io_timeout))?;
                reader
                    .get_mut()
                    .sock
                    .inner
                    .set_write_timeout(Some(inner.config.authenticated_io_timeout))?;
                write_message(reader.get_mut(), &hello(inner, peer, address)?)?;
                let (remote, cursor) = authenticate(
                    inner,
                    &handler,
                    read_message(&mut reader)?,
                    &peer.fingerprint,
                )?;
                send_batch(inner, &handler, &remote, &cursor, &mut reader)?;
                receive_batch(inner, &handler, &remote, &mut reader)?;
                return Ok(());
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}
fn inbound(socket: TcpStream, inner: &Arc<Inner>, cancel: &CancelToken) -> Result<()> {
    let _operation = OperationGuard::preauthenticated(inner, cancel, &socket)?;
    socket.set_nonblocking(false)?;
    socket.set_read_timeout(Some(Duration::from_secs(3)))?;
    socket.set_write_timeout(Some(Duration::from_secs(3)))?;
    let mut reader = BufReader::new(crate::sync::tls_server_stream(socket, &inner.keys)?);
    let message =
        crate::protocol::read_message_with_limit(&mut reader, crate::pairing::MAX_PAIRING_BYTES)?;
    let fp = crate::sync::device_fingerprint(reader.get_mut().conn.peer_certificates())?;
    cancel.check()?;
    let handler = context(inner)?;
    let identity = lock(&inner.envelope)?.identity.clone();
    match message {
        Message::PairHello(hello) => crate::pairing::accept_secure(
            &mut reader,
            hello,
            &identity,
            &inner.keys,
            &fp,
            &handler,
            |stream| {
                cancel.check()?;
                stream
                    .sock
                    .inner
                    .set_read_timeout(Some(inner.config.authenticated_io_timeout))?;
                stream
                    .sock
                    .inner
                    .set_write_timeout(Some(inner.config.authenticated_io_timeout))?;
                Ok(())
            },
        ),
        Message::PairRecover {
            invitation_id,
            identity: peer,
        } => crate::pairing::accept_recovery(
            &mut reader,
            &invitation_id,
            &peer,
            &identity,
            &fp,
            &handler,
            |stream| {
                cancel.check()?;
                stream
                    .sock
                    .inner
                    .set_read_timeout(Some(inner.config.authenticated_io_timeout))?;
                stream
                    .sock
                    .inner
                    .set_write_timeout(Some(inner.config.authenticated_io_timeout))?;
                Ok(())
            },
        ),
        message => {
            cancel.check()?;
            if let Message::ManagedHello { identity, .. } = &message {
                check(&*lock(&inner.envelope)?, &handler, identity, &fp)?;
            } else {
                return fail("expected authenticated managed hello");
            }
            reader
                .get_mut()
                .sock
                .inner
                .set_read_timeout(Some(inner.config.authenticated_io_timeout))?;
            reader
                .get_mut()
                .sock
                .inner
                .set_write_timeout(Some(inner.config.authenticated_io_timeout))?;
            let (peer, cursor) = authenticate(inner, &handler, message, &fp)?;
            let addr = lock(&inner.runtime)?
                .as_ref()
                .map(|r| r.address)
                .ok_or(Error::Cancelled)?;
            write_message(reader.get_mut(), &hello(inner, &peer, addr)?)?;
            receive_batch(inner, &handler, &peer, &mut reader)?;
            send_batch(inner, &handler, &peer, &cursor, &mut reader)
        }
    }
}
fn export(
    inner: &Inner,
    handler: &Handler,
    peer: &Peer,
    cursor: &ManagedReceipt,
) -> Result<ExportBatch> {
    inner.mutate(|e| {
        check(e, handler, &peer.identity, &peer.fingerprint)?;
        let full = cursor.epoch != e.epoch || cursor.sequence > e.revision;
        let records: Vec<_> = e
            .records
            .iter()
            .filter(|(k, _)| {
                full || (e.sequences.get(*k).copied().unwrap_or(u64::MAX) > cursor.sequence
                    && e.origins.get(*k) != Some(&peer.identity.device_id))
            })
            .map(|(_, r)| r.clone())
            .collect();
        let current = e
            .peers
            .get(&peer.identity.device_id)
            .ok_or(Error::Cancelled)?;
        let batch = ExportBatch {
            checkpoint: receipt::mint(inner, e, current)?,
            records: inner.export_records(&records)?,
            full,
        };
        limits::batch(&batch)?;
        Ok(batch)
    })
}
fn send_batch<R: BufRead + Write>(
    inner: &Inner,
    handler: &Handler,
    peer: &Peer,
    cursor: &ManagedReceipt,
    reader: &mut BufReader<R>,
) -> Result<()> {
    let batch = export(inner, handler, peer, cursor)?;
    let key = AppKey::from_slice(&lock(&inner.envelope)?.group_key)?;
    let plain = serde_json::to_vec(&batch)?;
    let encrypted = crate::encrypt_blob(&key, &plain, b"libresync-managed-delta-v1")?;
    drop(plain);
    write_message(reader.get_mut(), &Message::ManagedDelta { encrypted })?;
    inner.mutate(|e| {
        check(e, handler, &peer.identity, &peer.fingerprint)?;
        e.peers
            .get_mut(&peer.identity.device_id)
            .ok_or(Error::Cancelled)?
            .transmitted = batch.checkpoint.clone();
        Ok(())
    })?;
    match read_message(reader)? {
        Message::ManagedAck {
            receipt,
            disposition,
        } => {
            if receipt != batch.checkpoint {
                return fail("receipt does not match captured export");
            }
            inner.mutate(|e| {
                check(e, handler, &peer.identity, &peer.fingerprint)?;
                let current = e
                    .peers
                    .get(&peer.identity.device_id)
                    .ok_or(Error::Cancelled)?;
                receipt::verify(inner, e, current, &receipt)?;
                let p = e
                    .peers
                    .get_mut(&peer.identity.device_id)
                    .ok_or(Error::Cancelled)?;
                if p.stored.epoch != receipt.epoch || receipt.sequence > p.stored.sequence {
                    p.stored = receipt.clone();
                }
                if disposition == AckDisposition::Applied {
                    p.applied = receipt.clone();
                }
                p.state = if p.pending.is_some() {
                    PeerState::NeedsMerge
                } else if p.applied.epoch == receipt.epoch && p.applied.sequence >= receipt.sequence
                {
                    PeerState::UpToDate
                } else {
                    PeerState::Stored
                };
                Ok(())
            })
        }
        Message::ManagedPending => Ok(()),
        _ => fail("expected exact managed receipt"),
    }
}
fn receive_batch<R: BufRead + Write>(
    inner: &Inner,
    handler: &Handler,
    peer: &Peer,
    reader: &mut BufReader<R>,
) -> Result<()> {
    let encrypted = match read_message(reader)? {
        Message::ManagedDelta { encrypted } => encrypted,
        _ => return fail("expected managed delta"),
    };
    let key = AppKey::from_slice(&lock(&inner.envelope)?.group_key)?;
    if encrypted.len() > compact::MAX_CIPHERTEXT_BYTES {
        return fail_code(
            SessionErrorCode::InvalidRecord,
            "managed ciphertext exceeds batch bound",
        );
    }
    let plaintext = crate::decrypt_blob(&key, &encrypted, b"libresync-managed-delta-v1")?;
    if plaintext.len() > compact::MAX_BATCH_BYTES {
        return fail_code(
            SessionErrorCode::InvalidRecord,
            "managed plaintext exceeds batch bound",
        );
    }
    let batch: ExportBatch = serde_json::from_slice(&plaintext)?;
    drop(plaintext);
    limits::batch(&batch)?;
    let receipt = inner.mutate(|e| {
        check(e, handler, &peer.identity, &peer.fingerprint)?;
        let p = e
            .peers
            .get(&peer.identity.device_id)
            .ok_or(Error::Cancelled)?;
        if p.received.epoch == batch.checkpoint.epoch
            && p.received.sequence >= batch.checkpoint.sequence
        {
            return Ok(Some(batch.checkpoint.clone()));
        }
        validate_batch(e, &batch)?;
        if !p.bootstrapped && !batch.full {
            return fail("first managed batch must be an authoritative full snapshot");
        }
        inner.preview_records(e, &batch.records)?;
        if !p.bootstrapped && !e.records.is_empty() && !batch.records.is_empty() {
            let token = random_id();
            let preview = BootstrapPreview {
                token,
                peer: peer.identity.device_id.clone(),
                fingerprint: peer.fingerprint.clone(),
                local_revision: e.revision,
                batch: batch.clone(),
            };
            let p = e
                .peers
                .get_mut(&peer.identity.device_id)
                .ok_or(Error::Cancelled)?;
            if p.pending
                .as_ref()
                .is_none_or(|old| old.batch != batch || old.local_revision != e.revision)
            {
                p.pending = Some(preview);
            }
            p.state = PeerState::NeedsMerge;
            return Ok(None);
        }
        apply(inner, e, &batch, &peer.identity.device_id)?;
        Ok(Some(batch.checkpoint.clone()))
    })?;
    match receipt {
        Some(receipt) => write_message(
            reader.get_mut(),
            &Message::ManagedAck {
                receipt,
                disposition: AckDisposition::Stored,
            },
        ),
        None => {
            inner.emit(SessionEvent::Diagnostic {
                peer: Some(peer.identity.device_id.clone()),
                message: "Review incoming records before merging existing local data".into(),
                action: DiagnosticAction::ReviewMerge,
                evidence: DiagnosticEvidence::BootstrapRequired,
            });
            write_message(reader.get_mut(), &Message::ManagedPending)
        }
    }
}
fn validate_batch(e: &Envelope, batch: &ExportBatch) -> Result<()> {
    if batch.checkpoint.epoch.is_empty()
        || batch.checkpoint.epoch.len() > 128
        || batch.checkpoint.proof.len() != 32
        || batch.records.len() > 100_000
    {
        return fail("invalid managed batch");
    }
    let mut ids = std::collections::BTreeSet::new();
    for r in &batch.records {
        if r.id.is_empty()
            || r.id.len() > 1024
            || r.value.len() > compact::MAX_RECORD_BYTES
            || !ids.insert(record_key(&r.adapter, &r.id)?)
            || !e
                .metadata
                .manifest
                .adapters
                .iter()
                .any(|a| a.id == r.adapter)
        {
            return fail("invalid managed record");
        }
    }
    Ok(())
}
fn apply(inner: &Inner, e: &mut Envelope, batch: &ExportBatch, peer: &str) -> Result<()> {
    let prepared = inner.prepare_records(e, &batch.records)?;
    inner.commit_records(e, &prepared, Some(peer))?;
    let p = e.peers.get_mut(peer).ok_or(Error::Cancelled)?;
    p.received = batch.checkpoint.clone();
    p.bootstrapped = true;
    p.pending = None;
    Ok(())
}

impl Session {
    pub fn bootstrap_preview(&self, peer: &str) -> Result<Option<BootstrapPreview>> {
        Ok(lock(&self.inner.envelope)?
            .peers
            .get(peer)
            .ok_or_else(|| Error::Protocol("unknown peer".into()))?
            .pending
            .clone())
    }
    pub fn resolve_bootstrap(
        &self,
        preview: &BootstrapPreview,
        decision: BootstrapDecision,
    ) -> Result<()> {
        self.inner.mutate(|e| {
            let p = e.peers.get(&preview.peer).ok_or(Error::Cancelled)?;
            if p.revoked
                || p.fingerprint != preview.fingerprint
                || e.revision != preview.local_revision
                || p.pending
                    .as_ref()
                    .is_none_or(|v| v.token != preview.token || v.batch != preview.batch)
            {
                return fail_code(SessionErrorCode::StaleBootstrap, "stale bootstrap preview");
            }
            match decision {
                BootstrapDecision::Cancel => {
                    let p = e.peers.get_mut(&preview.peer).ok_or(Error::Cancelled)?;
                    p.pending = None;
                    p.state = PeerState::Paused;
                }
                BootstrapDecision::Merge => {
                    validate_batch(e, &preview.batch)?;
                    apply(&self.inner, e, &preview.batch, &preview.peer)?;
                }
            }
            Ok(())
        })?;
        self.inner.wake();
        Ok(())
    }
    pub fn acknowledge_applied(&self, peer: &str, receipt: &ManagedReceipt) -> Result<()> {
        self.inner.mutate(|e| {
            let p = e.peers.get_mut(peer).ok_or(Error::Cancelled)?;
            if p.revoked
                || p.received.epoch != receipt.epoch
                || receipt.sequence > p.received.sequence
            {
                return fail("processing receipt is outside stored source history");
            }
            if p.processed.epoch != receipt.epoch || receipt.sequence > p.processed.sequence {
                p.processed = receipt.clone();
            }
            Ok(())
        })?;
        self.inner.wake();
        Ok(())
    }
    pub fn stored_inbound_receipt(&self, peer: &str) -> Result<ManagedReceipt> {
        Ok(lock(&self.inner.envelope)?
            .peers
            .get(peer)
            .ok_or(Error::Cancelled)?
            .received
            .clone())
    }
}
impl Session {
    pub fn remove_peer(&self, peer: &str) -> Result<()> {
        let running = lock(&self.inner.envelope)?.phase == SessionPhase::Running;
        self.inner.mutate(|e| {
            let p = e
                .peers
                .get_mut(peer)
                .ok_or_else(|| Error::Protocol("unknown peer".into()))?;
            p.revoked = true;
            p.incarnation = random_id();
            p.state = PeerState::Paused;
            p.pending = None;
            e.generation = e
                .generation
                .checked_add(1)
                .ok_or_else(|| Error::Protocol("generation exhausted".into()))?;
            e.enrollments
                .retain(|_, j| j.outcome.identity.device_id != peer);
            e.preparations
                .retain(|_, j| j.outcome.identity.device_id != peer);
            Ok(())
        })?;
        self.shutdown()?;
        if running {
            self.start()?;
        }
        Ok(())
    }
    pub fn pause_peer(&self, peer: &str) -> Result<()> {
        let running = lock(&self.inner.envelope)?.phase == SessionPhase::Running;
        self.inner.mutate(|e| {
            e.peers.get_mut(peer).ok_or(Error::Cancelled)?.state = PeerState::Paused;
            e.generation = e
                .generation
                .checked_add(1)
                .ok_or_else(|| Error::Protocol("generation exhausted".into()))?;
            Ok(())
        })?;
        self.shutdown()?;
        if running {
            self.start()?;
        }
        Ok(())
    }
    pub fn resume_peer(&self, peer: &str) -> Result<()> {
        self.inner.mutate(|e| {
            let p = e.peers.get_mut(peer).ok_or(Error::Cancelled)?;
            if p.revoked {
                return fail("removed peer requires explicit repair");
            }
            p.state = PeerState::Waiting;
            Ok(())
        })?;
        self.inner.wake();
        Ok(())
    }
    /// Explicit repair authorizes the invitation's pinned replacement certificate.
    /// Existing local records and pending edits are preserved.
    pub fn repair_peer(
        &self,
        peer: &str,
        invitation: &SessionInvitation,
    ) -> Result<crate::SecurePairingOutcome> {
        if invitation.identity.device_id != peer {
            return fail("repair invitation belongs to another device");
        }
        self.remove_peer(peer)?;
        self.connect_inner(invitation, true)
    }
}

impl Inner {
    pub(super) fn refresh_advertisement(&self) -> Result<()> {
        if !self.config.advertise {
            return Ok(());
        }
        let identity = lock(&self.envelope)?.identity.clone();
        let descriptor = lock(&self.invitation)?.clone();
        let mut runtime = lock(&self.runtime)?;
        let Some(runtime) = runtime.as_mut() else {
            return Ok(());
        };
        if let Some(advertiser) = &runtime.advertiser {
            advertiser.refresh_managed(
                &identity,
                runtime.address,
                &self.config.metadata,
                descriptor.as_ref(),
            )?;
        }
        Ok(())
    }
}
impl Session {
    pub fn discovered_peers(&self) -> Result<Vec<crate::DiscoveredPeer>> {
        Ok(lock(&self.inner.discovered)?.values().cloned().collect())
    }
    pub fn close_pairing(&self) -> Result<()> {
        let _lifecycle = lock(&self.inner.lifecycle)?;
        self.inner.pairing.close()?;
        *lock(&self.inner.invitation)? = None;
        self.inner.refresh_advertisement()
    }
    pub fn connect_code(
        &self,
        peer: &crate::DiscoveredPeer,
        code: &str,
    ) -> Result<crate::SecurePairingOutcome> {
        let ad = peer
            .advertisement
            .as_ref()
            .ok_or_else(|| Error::Protocol("peer has no managed metadata".into()))?;
        if peer.identity.app_id != self.inner.config.metadata.manifest.app_id
            || !ad.compatible_with(&self.inner.config.metadata.manifest)?
        {
            return fail("incompatible discovered app schema");
        }
        let descriptor = peer
            .invitation
            .as_ref()
            .ok_or_else(|| Error::Protocol("peer pairing is closed".into()))?;
        let metadata = DeviceMetadata {
            display_name: ad.display_name.clone(),
            device_kind: ad.device_kind.clone(),
            role: ad.role.clone(),
            manifest: self.inner.config.metadata.manifest.clone(),
        };
        self.connect(&SessionInvitation {
            invitation: descriptor.with_code(metadata, code)?,
            identity: peer.identity.clone(),
            addresses: peer.addresses.clone(),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AdapterDescriptor, AppManifest, DeviceMetadata, MemoryKeyStore};
    #[test]
    #[ignore = "large populated pairing acceptance: run explicitly in the optimized release gate"]
    fn populated_32_mib_inviter_and_joiner_pair_with_short_initial_deadlines() {
        let _large = LARGE_FIXTURE.lock().unwrap();
        let ad = tempfile::tempdir().unwrap();
        let bd = tempfile::tempdir().unwrap();
        let a = Session::open(config(ad.path()), Arc::new(MemoryKeyStore::new())).unwrap();
        let b = Session::open(config(bd.path()), Arc::new(MemoryKeyStore::new())).unwrap();
        a.set("records", "inviter-snapshot", vec![255; 32 * 1024 * 1024])
            .unwrap();
        b.set("records", "joiner-snapshot", vec![254; 32 * 1024 * 1024])
            .unwrap();
        a.start().unwrap();
        b.start().unwrap();
        let invitation = a.create_invitation(Duration::from_secs(300)).unwrap();
        b.connect(&invitation).unwrap();
        let aid = lock(&a.inner.envelope).unwrap().identity.device_id.clone();
        let bid = lock(&b.inner.envelope).unwrap().identity.device_id.clone();
        assert!(!lock(&a.inner.envelope).unwrap().peers[&bid].revoked);
        assert!(!lock(&b.inner.envelope).unwrap().peers[&aid].revoked);
        b.pause().unwrap();
        a.pause().unwrap();
        assert_eq!(
            lock(&a.inner.envelope).unwrap().records.len(),
            1,
            "pairing bypassed populated merge consent"
        );
        assert_eq!(
            lock(&b.inner.envelope).unwrap().records.len(),
            1,
            "pairing bypassed populated merge consent"
        );
    }
    #[test]
    fn repeated_shutdown_preserves_journal_and_generation_after_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let session = Session::open(config(dir.path()), Arc::new(MemoryKeyStore::new())).unwrap();
        session.start().unwrap();
        let running = lock(&session.inner.envelope).unwrap().generation;
        session.shutdown().unwrap();
        let stopped = lock(&session.inner.envelope).unwrap().generation;
        assert!(stopped > running);
        let bytes = std::fs::read(dir.path().join("session.enc")).unwrap();
        session.shutdown().unwrap();
        assert_eq!(lock(&session.inner.envelope).unwrap().generation, stopped);
        assert_eq!(
            std::fs::read(dir.path().join("session.enc")).unwrap(),
            bytes
        );
        assert!(lock(&session.inner.runtime).unwrap().is_none());
        assert!(lock(&session.inner.operations).unwrap().is_empty());
    }
    #[test]
    fn initial_tls_and_socket_cancellation_do_not_wait_for_coordinator_commit() {
        let dir = tempfile::tempdir().unwrap();
        let session = Session::open(config(dir.path()), Arc::new(MemoryKeyStore::new())).unwrap();
        let address = session.start().unwrap();
        let cancel = lock(&session.inner.runtime)
            .unwrap()
            .as_ref()
            .unwrap()
            .cancel
            .clone();
        // A blocked durable commit must not prevent initial TLS or socket registration.
        let envelope = lock(&session.inner.envelope).unwrap();
        let socket = TcpStream::connect(address).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let client = crate::sync::tls_client_stream(socket, &session.inner.keys).unwrap();
        assert_eq!(lock(&session.inner.operations).unwrap().len(), 1);
        cancel.cancel();
        for token in lock(&session.inner.operations).unwrap().values() {
            token.cancel();
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        while !lock(&session.inner.operations).unwrap().is_empty() {
            assert!(
                Instant::now() < deadline,
                "pre-auth socket remained blocked behind coordinator"
            );
            thread::sleep(Duration::from_millis(5));
        }
        drop(client);
        drop(envelope);
        session.shutdown().unwrap();
    }
    #[test]
    fn compact_record_edges_and_atomic_aggregate_overflow_preserve_state() {
        let _large = LARGE_FIXTURE.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let a = Session::open(config(dir.path()), Arc::new(MemoryKeyStore::new())).unwrap();
        a.set("records", "existing", vec![255; 10 * 1024 * 1024])
            .unwrap();
        let before = lock(&a.inner.envelope).unwrap().clone();
        let durable = std::fs::read(dir.path().join("session.enc")).unwrap();
        let incoming = ExportBatch {
            checkpoint: ManagedReceipt {
                epoch: "budget-source".into(),
                sequence: 1,
                proof: vec![0; 32],
            },
            full: true,
            records: (0..2)
                .map(|i| ManagedRecord {
                    adapter: "records".into(),
                    id: format!("new-{i}"),
                    value: vec![255; 44 * 1024 * 1024],
                    deleted: false,
                    clock: LamportClock {
                        counter: i + 1,
                        device_id: "remote".into(),
                    },
                })
                .collect(),
        };
        limits::batch(&incoming).unwrap();
        assert!(a
            .inner
            .mutate(|e| apply(&a.inner, e, &incoming, "remote"))
            .is_err());
        assert!(*lock(&a.inner.envelope).unwrap() == before);
        assert_eq!(
            std::fs::read(dir.path().join("session.enc")).unwrap(),
            durable
        );
        assert!(a
            .set(
                "records",
                "over-record",
                vec![0; compact::MAX_RECORD_BYTES + 1]
            )
            .is_err());
        let mut edge = before.clone();
        edge.records
            .get_mut(&record_key("records", "existing").unwrap())
            .unwrap()
            .value
            .resize(compact::MAX_RECORD_BYTES, 255);
        limits::state(&edge).unwrap();
        edge.records
            .get_mut(&record_key("records", "existing").unwrap())
            .unwrap()
            .value
            .push(0);
        assert!(limits::state(&edge).is_err());
        let mut imported = before.clone();
        for record in incoming.records {
            imported
                .records
                .insert(record_key(&record.adapter, &record.id).unwrap(), record);
        }
        assert!(recover_transaction(
            &a.inner.config,
            &before,
            &PreparedTransaction {
                id: "overflow".into(),
                changes: Vec::new(),
                target: Box::new(imported)
            }
        )
        .is_err());
        assert!(*lock(&a.inner.envelope).unwrap() == before);
    }
    static LARGE_FIXTURE: Mutex<()> = Mutex::new(());
    fn prove_large_application_receipts(a: &Session, b: &Session, budget: Duration) {
        let bid = lock(&b.inner.envelope).unwrap().identity.device_id.clone();
        let aid = lock(&a.inner.envelope).unwrap().identity.device_id.clone();
        let inbox = b.application_inbox().unwrap();
        let captured = inbox.receipts[&aid].clone();
        let deadline = Instant::now() + budget;
        loop {
            let e = lock(&a.inner.envelope).unwrap();
            if e.peers[&bid].stored.epoch == captured.epoch
                && e.peers[&bid].stored.sequence >= captured.sequence
            {
                assert!(
                    e.peers[&bid].applied.epoch != captured.epoch
                        || e.peers[&bid].applied.sequence < captured.sequence,
                    "Stored falsely claimed Applied"
                );
                break;
            }
            drop(e);
            assert!(Instant::now() < deadline, "large Stored receipt missing");
            thread::sleep(Duration::from_millis(20));
        }
        b.acknowledge_inbox(&inbox).unwrap();
        drop(inbox);
        let deadline = Instant::now() + budget;
        loop {
            let e = lock(&a.inner.envelope).unwrap();
            if e.peers[&bid].applied.epoch == captured.epoch
                && e.peers[&bid].applied.sequence >= captured.sequence
            {
                break;
            }
            drop(e);
            assert!(Instant::now() < deadline, "large Applied receipt missing");
            thread::sleep(Duration::from_millis(20));
        }
    }
    #[test]
    #[ignore = "large payload acceptance: run explicitly in the optimized release gate"]
    fn compact_32_mib_snapshot_crosses_real_tls_and_aggregate_overflow_is_atomic() {
        let _large = LARGE_FIXTURE.lock().unwrap();
        let ad = tempfile::tempdir().unwrap();
        let bd = tempfile::tempdir().unwrap();
        let a = Session::open(config(ad.path()), Arc::new(MemoryKeyStore::new())).unwrap();
        let b = Session::open(config(bd.path()), Arc::new(MemoryKeyStore::new())).unwrap();
        pair(&a, &b);
        a.set("records", "snapshot", vec![255; 32 * 1024 * 1024])
            .unwrap();
        b.resume().unwrap();
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let received = lock(&b.inner.envelope)
                .unwrap()
                .records
                .get(&record_key("records", "snapshot").unwrap())
                .is_some_and(|r| {
                    r.value.len() == 32 * 1024 * 1024 && r.value.iter().all(|b| *b == 255)
                });
            if received {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "32MiB snapshot did not cross real TLS; source={:?}; destination={:?}",
                lock(&a.inner.diagnostics).unwrap(),
                lock(&b.inner.diagnostics).unwrap()
            );
            thread::sleep(Duration::from_millis(20));
        }
        prove_large_application_receipts(&a, &b, Duration::from_secs(120));
        b.pause().unwrap();
        let bid = b.snapshot().unwrap().identity.device_id;
        let peer = lock(&a.inner.envelope).unwrap().peers[&bid].clone();
        let handler = context(&a.inner).unwrap();
        let batch = export(&a.inner, &handler, &peer, &ManagedReceipt::default()).unwrap();
        let plaintext = serde_json::to_vec(&batch).unwrap();
        assert!(plaintext.len() < compact::MAX_BATCH_BYTES);
        let key = AppKey::from_slice(&lock(&a.inner.envelope).unwrap().group_key).unwrap();
        let frame = serde_json::to_vec(&Message::ManagedDelta {
            encrypted: crate::encrypt_blob(&key, &plaintext, b"libresync-managed-delta-v1")
                .unwrap(),
        })
        .unwrap();
        assert!(frame.len() < crate::protocol::MAX_MESSAGE_BYTES as usize);
        drop(frame);
        drop(plaintext);
        drop(batch);
        a.pause().unwrap();
        let before = lock(&a.inner.envelope).unwrap().clone();
        let incoming = ExportBatch {
            checkpoint: ManagedReceipt {
                epoch: "incoming-budget-test".into(),
                sequence: 10,
                proof: vec![0; 32],
            },
            full: true,
            records: (0..2)
                .map(|i| ManagedRecord {
                    adapter: "records".into(),
                    id: format!("incoming-{i}"),
                    value: vec![255; 32 * 1024 * 1024],
                    deleted: false,
                    clock: LamportClock {
                        counter: i + 1,
                        device_id: bid.clone(),
                    },
                })
                .collect(),
        };
        limits::batch(&incoming).unwrap();
        assert!(a
            .inner
            .mutate(|e| apply(&a.inner, e, &incoming, &bid))
            .is_err());
        assert!(*lock(&a.inner.envelope).unwrap() == before);
        assert!(a
            .set("records", "too-much-state", vec![255; 64 * 1024 * 1024])
            .is_err());
        assert!(*lock(&a.inner.envelope).unwrap() == before);
        assert!(a
            .set(
                "records",
                "too-large-record",
                vec![0; compact::MAX_RECORD_BYTES + 1]
            )
            .is_err());
        assert!(*lock(&a.inner.envelope).unwrap() == before);
        let mut edge = before.clone();
        let value = &mut edge
            .records
            .get_mut(&record_key("records", "snapshot").unwrap())
            .unwrap()
            .value;
        value.resize(compact::MAX_RECORD_BYTES, 255);
        limits::state(&edge).unwrap();
        edge.records
            .get_mut(&record_key("records", "snapshot").unwrap())
            .unwrap()
            .value
            .push(0);
        assert!(limits::state(&edge).is_err());
        drop(edge);
        let mut imported = before.clone();
        for record in incoming.records {
            imported
                .records
                .insert(record_key(&record.adapter, &record.id).unwrap(), record);
        }
        let journal = PreparedTransaction {
            id: "oversized-import".into(),
            changes: Vec::new(),
            target: Box::new(imported),
        };
        assert!(recover_transaction(&a.inner.config, &before, &journal).is_err());
    }
    #[test]
    #[ignore = "large payload acceptance: run explicitly in the optimized release gate"]
    fn compact_64_mib_record_and_near_aggregate_limit_cross_real_tls() {
        let _large = LARGE_FIXTURE.lock().unwrap();
        let ad = tempfile::tempdir().unwrap();
        let bd = tempfile::tempdir().unwrap();
        let mut ac = config(ad.path());
        let mut bc = config(bd.path());
        if cfg!(debug_assertions) {
            ac.authenticated_io_timeout = Duration::from_secs(180);
            bc.authenticated_io_timeout = Duration::from_secs(180);
        }
        let a = Session::open(ac, Arc::new(MemoryKeyStore::new())).unwrap();
        let b = Session::open(bc, Arc::new(MemoryKeyStore::new())).unwrap();
        pair(&a, &b);
        a.set("records", "snapshot", vec![255; compact::MAX_RECORD_BYTES])
            .unwrap();
        a.set("records", "tail", vec![254; 31 * 1024 * 1024])
            .unwrap();
        b.resume().unwrap();
        let deadline = Instant::now() + Duration::from_secs(240);
        loop {
            let e = lock(&b.inner.envelope).unwrap();
            let complete = e
                .records
                .get(&record_key("records", "snapshot").unwrap())
                .is_some_and(|r| {
                    r.value.len() == compact::MAX_RECORD_BYTES && r.value.iter().all(|b| *b == 255)
                })
                && e.records
                    .get(&record_key("records", "tail").unwrap())
                    .is_some_and(|r| {
                        r.value.len() == 31 * 1024 * 1024 && r.value.iter().all(|b| *b == 254)
                    });
            drop(e);
            if complete {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "near-limit authoritative TLS delivery missing"
            );
            thread::sleep(Duration::from_millis(20));
        }
        prove_large_application_receipts(&a, &b, Duration::from_secs(240));
        b.shutdown().unwrap();
        a.shutdown().unwrap();
    }
    #[test]
    fn checkpoint_proofs_bound_history_survive_restart_and_reject_reenrollment_replay() {
        let ad = tempfile::tempdir().unwrap();
        let bd = tempfile::tempdir().unwrap();
        let keys = Arc::new(MemoryKeyStore::new());
        let a = Session::open(config(ad.path()), keys.clone()).unwrap();
        let b = Session::open(config(bd.path()), Arc::new(MemoryKeyStore::new())).unwrap();
        pair(&a, &b);
        a.set("records", "x", b"first".to_vec()).unwrap();
        let handler = context(&a.inner).unwrap();
        let bid = b.snapshot().unwrap().identity.device_id;
        let peer = lock(&a.inner.envelope).unwrap().peers[&bid].clone();
        let old = export(&a.inner, &handler, &peer, &ManagedReceipt::default())
            .unwrap()
            .checkpoint;
        let before = serde_json::to_vec(&*lock(&a.inner.envelope).unwrap())
            .unwrap()
            .len();
        for i in 0..100 {
            a.set("records", "x", i.to_string().into_bytes()).unwrap();
            export(&a.inner, &handler, &peer, &ManagedReceipt::default()).unwrap();
        }
        let envelope = lock(&a.inner.envelope).unwrap().clone();
        assert!(
            serde_json::to_vec(&envelope).unwrap().len() < before + 2048,
            "Stored-only checkpoint history grew unbounded"
        );
        receipt::verify(&a.inner, &envelope, &envelope.peers[&bid], &old).unwrap();
        let mut forged = old.clone();
        forged.sequence += 1;
        assert!(receipt::verify(&a.inner, &envelope, &envelope.peers[&bid], &forged).is_err());
        assert!(receipt::verify(
            &a.inner,
            &envelope,
            &envelope.peers[&bid],
            &ManagedReceipt::default()
        )
        .is_err());
        let (_, cursor) = authenticate(
            &a.inner,
            &handler,
            Message::ManagedHello {
                identity: peer.identity.clone(),
                metadata: peer.metadata.clone(),
                cursor: forged.clone(),
                processed: forged.clone(),
                addresses: Vec::new(),
            },
            &peer.fingerprint,
        )
        .unwrap();
        assert_eq!(
            cursor,
            ManagedReceipt::default(),
            "forged cursor suppressed full export"
        );
        assert_eq!(
            lock(&a.inner.envelope).unwrap().peers[&bid].applied,
            envelope.peers[&bid].applied,
            "stale processed proof cleared pending data"
        );
        assert!(export(&a.inner, &handler, &peer, &cursor).unwrap().full);
        authenticate(
            &a.inner,
            &handler,
            Message::ManagedHello {
                identity: peer.identity.clone(),
                metadata: peer.metadata.clone(),
                cursor: old.clone(),
                processed: old.clone(),
                addresses: Vec::new(),
            },
            &peer.fingerprint,
        )
        .unwrap();
        {
            let e = lock(&a.inner.envelope).unwrap();
            assert_eq!(
                e.peers[&bid].stored, old,
                "lost Stored response was not recovered from genuine Applied proof"
            );
            assert_eq!(e.peers[&bid].applied, old);
            assert_eq!(
                pending(&a.inner, &e, &e.peers[&bid]).unwrap(),
                1,
                "delayed Applied proof cleared newer edits"
            );
        }
        a.shutdown().unwrap();
        drop(handler);
        drop(a);
        let reopened = Session::open(config(ad.path()), keys).unwrap();
        {
            let envelope = lock(&reopened.inner.envelope).unwrap();
            receipt::verify(&reopened.inner, &envelope, &envelope.peers[&bid], &old).unwrap();
        }
        reopened.start().unwrap();
        b.resume().unwrap();
        reopened
            .repair_peer(&bid, &b.create_invitation(Duration::from_secs(30)).unwrap())
            .unwrap();
        let envelope = lock(&reopened.inner.envelope).unwrap();
        assert!(
            receipt::verify(&reopened.inner, &envelope, &envelope.peers[&bid], &old).is_err(),
            "pre-repair proof replayed into new enrollment"
        );
        assert_ne!(envelope.peers[&bid].stored.proof, old.proof);
        assert_eq!(envelope.records.len(), 1);
    }
    #[test]
    fn authoritative_export_includes_retained_destination_records_for_missing_and_future_cursors() {
        let ad = tempfile::tempdir().unwrap();
        let bd = tempfile::tempdir().unwrap();
        let a = Session::open(config(ad.path()), Arc::new(MemoryKeyStore::new())).unwrap();
        let b = Session::open(config(bd.path()), Arc::new(MemoryKeyStore::new())).unwrap();
        pair(&a, &b);
        a.set("records", "retained", b"restorable".to_vec())
            .unwrap();
        let peer_id = b.snapshot().unwrap().identity.device_id;
        a.inner
            .mutate(|e| {
                e.origins
                    .insert(record_key("records", "retained")?, peer_id.clone());
                Ok(())
            })
            .unwrap();
        let handler = context(&a.inner).unwrap();
        let peer = lock(&a.inner.envelope).unwrap().peers[&peer_id].clone();
        let current = lock(&a.inner.envelope).unwrap().clone();
        for cursor in [
            ManagedReceipt::default(),
            ManagedReceipt {
                epoch: current.epoch.clone(),
                sequence: current.revision + 1,
                ..ManagedReceipt::default()
            },
        ] {
            let batch = export(&a.inner, &handler, &peer, &cursor).unwrap();
            assert!(batch.full);
            assert!(
                batch.records.iter().any(|r| r.id == "retained"),
                "full export omitted restored data"
            );
        }
    }
    #[test]
    fn diagnostics_preserve_typed_failures_and_do_not_infer_network_cause() {
        let dir = tempfile::tempdir().unwrap();
        let session = Session::open(config(dir.path()), Arc::new(MemoryKeyStore::new())).unwrap();
        let events = session.subscribe().unwrap();
        for (code, action) in [
            (
                SessionErrorCode::IncompatibleSchema,
                DiagnosticAction::ReviewCompatibility,
            ),
            (
                SessionErrorCode::InvalidRecord,
                DiagnosticAction::ReviewRecords,
            ),
            (
                SessionErrorCode::GroupConflict,
                DiagnosticAction::ResolveGroupConflict,
            ),
            (SessionErrorCode::Busy, DiagnosticAction::Wait),
            (
                SessionErrorCode::StorageCommitUncertain,
                DiagnosticAction::CheckSecureStorage,
            ),
        ] {
            session.inner.diagnostic(
                None,
                &Error::Managed {
                    code: code.clone(),
                    message: "test".into(),
                },
            );
            let SessionEvent::Diagnostic {
                action: actual,
                evidence,
                ..
            } = events.recv().unwrap()
            else {
                panic!("missing diagnostic")
            };
            assert_eq!(actual, action);
            match evidence {
                DiagnosticEvidence::ManagedFailure { code: actual } => assert_eq!(actual, code),
                DiagnosticEvidence::StorageCommitUncertain
                    if code == SessionErrorCode::StorageCommitUncertain => {}
                _ => panic!("incorrect evidence"),
            }
        }
        session.inner.diagnostic(
            None,
            &Error::Io(std::io::Error::other("unspecified operation failure")),
        );
        assert!(matches!(
            events.recv().unwrap(),
            SessionEvent::Diagnostic {
                evidence: DiagnosticEvidence::Unknown,
                ..
            }
        ));
        session
            .inner
            .storage_uncertain
            .store(true, std::sync::atomic::Ordering::SeqCst);
        session
            .inner
            .diagnostic(None, &Error::Io(std::io::Error::other("fsync")));
        assert!(matches!(
            events.recv().unwrap(),
            SessionEvent::Diagnostic {
                evidence: DiagnosticEvidence::StorageCommitUncertain,
                action: DiagnosticAction::CheckSecureStorage,
                ..
            }
        ));
    }
    fn config(path: &std::path::Path) -> SessionConfig {
        let mut c = SessionConfig::new(
            path,
            DeviceMetadata {
                display_name: "test".into(),
                device_kind: "desktop".into(),
                role: "application".into(),
                manifest: AppManifest {
                    app_id: "test.app".into(),
                    display_name: "Test".into(),
                    schema_version: 1,
                    adapters: vec![AdapterDescriptor {
                        id: "records".into(),
                        namespace: "test.app".into(),
                        schema: "kv".into(),
                        transactional: true,
                    }],
                },
            },
        );
        c.advertise = false;
        c.listen = SocketAddr::from(([127, 0, 0, 1], 0));
        c
    }
    fn pair(a: &Session, b: &Session) {
        a.start().unwrap();
        b.start().unwrap();
        b.connect(&a.create_invitation(Duration::from_secs(30)).unwrap())
            .unwrap();
        b.pause().unwrap();
    }
    fn socket(a: &Session, b: &Session) -> BufReader<crate::sync::ClientStream> {
        let id = b.snapshot().unwrap().identity;
        let mut reader = crate::sync::open_client(
            a.address().unwrap(),
            &b.inner.keys,
            &SyncOptions::new()
                .with_expected_fingerprint(a.inner.keys.fingerprint())
                .with_timeouts(Duration::from_secs(1), Duration::from_secs(2)),
            &crate::sync::ByteCounters::default(),
        )
        .unwrap();
        write_message(
            reader.get_mut(),
            &Message::ManagedHello {
                identity: id,
                metadata: b.inner.config.metadata.clone(),
                cursor: ManagedReceipt::default(),
                processed: ManagedReceipt::default(),
                addresses: vec![],
            },
        )
        .unwrap();
        assert!(matches!(
            read_message(&mut reader).unwrap(),
            Message::ManagedHello { .. }
        ));
        reader
    }
    fn send(reader: &mut BufReader<crate::sync::ClientStream>, a: &Session, batch: &ExportBatch) {
        let key = AppKey::from_slice(&lock(&a.inner.envelope).unwrap().group_key).unwrap();
        write_message(
            reader.get_mut(),
            &Message::ManagedDelta {
                encrypted: crate::encrypt_blob(
                    &key,
                    &serde_json::to_vec(batch).unwrap(),
                    b"libresync-managed-delta-v1",
                )
                .unwrap(),
            },
        )
        .unwrap();
    }
    #[test]
    fn captured_socket_export_receipt_does_not_clear_a_concurrent_local_edit() {
        let ad = tempfile::tempdir().unwrap();
        let bd = tempfile::tempdir().unwrap();
        let a = Session::open(config(ad.path()), Arc::new(MemoryKeyStore::new())).unwrap();
        let b = Session::open(config(bd.path()), Arc::new(MemoryKeyStore::new())).unwrap();
        pair(&a, &b);
        a.set("records", "x", b"first".to_vec()).unwrap();
        let mut reader = socket(&a, &b);
        send(
            &mut reader,
            &a,
            &ExportBatch {
                checkpoint: ManagedReceipt {
                    epoch: "b-history".into(),
                    sequence: 0,
                    proof: vec![0; 32],
                },
                records: vec![],
                full: true,
            },
        );
        assert!(matches!(
            read_message(&mut reader).unwrap(),
            Message::ManagedAck { .. }
        ));
        let encrypted = match read_message(&mut reader).unwrap() {
            Message::ManagedDelta { encrypted } => encrypted,
            _ => panic!("delta"),
        };
        let key = AppKey::from_slice(&lock(&a.inner.envelope).unwrap().group_key).unwrap();
        let captured: ExportBatch = serde_json::from_slice(
            &crate::decrypt_blob(&key, &encrypted, b"libresync-managed-delta-v1").unwrap(),
        )
        .unwrap();
        a.set("records", "x", b"second".to_vec()).unwrap();
        write_message(
            reader.get_mut(),
            &Message::ManagedAck {
                receipt: captured.checkpoint.clone(),
                disposition: AckDisposition::Stored,
            },
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while a.snapshot().unwrap().peers[0].stored != captured.checkpoint {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(a.snapshot().unwrap().peers[0].pending, 1);
        assert_eq!(a.snapshot().unwrap().local_revision, 2);
    }
    #[test]
    fn removal_cancels_held_authenticated_socket_and_fences_late_receive() {
        let ad = tempfile::tempdir().unwrap();
        let bd = tempfile::tempdir().unwrap();
        let a = Session::open(config(ad.path()), Arc::new(MemoryKeyStore::new())).unwrap();
        let b = Session::open(config(bd.path()), Arc::new(MemoryKeyStore::new())).unwrap();
        pair(&a, &b);
        let mut reader = socket(&a, &b);
        let before = Instant::now();
        a.remove_peer(&b.snapshot().unwrap().identity.device_id)
            .unwrap();
        assert!(before.elapsed() < Duration::from_secs(1));
        let batch = ExportBatch {
            checkpoint: ManagedReceipt {
                epoch: "b-history".into(),
                sequence: 1,
                proof: vec![0; 32],
            },
            records: vec![ManagedRecord {
                adapter: "records".into(),
                id: "late".into(),
                value: b"late".to_vec(),
                deleted: false,
                clock: LamportClock {
                    counter: 1,
                    device_id: b.snapshot().unwrap().identity.device_id,
                },
            }],
            full: true,
        };
        let key = AppKey::from_slice(&lock(&a.inner.envelope).unwrap().group_key).unwrap();
        let _ = write_message(
            reader.get_mut(),
            &Message::ManagedDelta {
                encrypted: crate::encrypt_blob(
                    &key,
                    &serde_json::to_vec(&batch).unwrap(),
                    b"libresync-managed-delta-v1",
                )
                .unwrap(),
            },
        );
        assert!(read_message(&mut reader).is_err());
        assert_eq!(a.get("records", "late").unwrap(), None);
    }
    #[test]
    fn shutdown_joins_stalled_unauthenticated_handshakes() {
        let ad = tempfile::tempdir().unwrap();
        let a = Session::open(config(ad.path()), Arc::new(MemoryKeyStore::new())).unwrap();
        a.start().unwrap();
        let sockets: Vec<_> = (0..4)
            .map(|_| TcpStream::connect(a.address().unwrap()).unwrap())
            .collect();
        thread::sleep(Duration::from_millis(30));
        let before = Instant::now();
        a.shutdown().unwrap();
        assert!(before.elapsed() < Duration::from_secs(1));
        assert!(lock(&a.inner.operations).unwrap().is_empty());
        drop(sockets);
    }
    #[test]
    fn worker_panic_still_joins_remaining_workers_and_releases_runtime_lease() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let ad = tempfile::tempdir().unwrap();
        let keys = Arc::new(MemoryKeyStore::new());
        let mut c = config(ad.path());
        c.advertise = true;
        let a = Session::open(c, keys.clone()).unwrap();
        a.start().unwrap();
        let finished = Arc::new(AtomicBool::new(false));
        let browser = {
            let mut runtime = lock(&a.inner.runtime).unwrap();
            let r = runtime.as_mut().unwrap();
            let browser = r.advertiser.as_ref().unwrap().browse_managed().unwrap();
            let original = std::mem::replace(
                &mut r.scheduler,
                thread::spawn(|| panic!("injected scheduler panic")),
            );
            let inner = a.inner.clone();
            let cancel = r.cancel.clone();
            let done = finished.clone();
            lock(&r.workers).unwrap().extend([
                original,
                thread::spawn(|| panic!("injected inbound panic")),
                thread::spawn(move || {
                    while !cancel.is_cancelled() {
                        thread::sleep(Duration::from_millis(1));
                    }
                    thread::sleep(Duration::from_millis(80));
                    drop(inner);
                    done.store(true, Ordering::SeqCst);
                }),
            ]);
            browser
        };
        let failure = a.shutdown().unwrap_err().to_string();
        assert!(failure.contains("scheduler worker panicked"));
        assert!(failure.contains("inbound worker panicked"));
        // The upstream daemon closes its command channel and acknowledges
        // cleanup just before its final scope drops the browse senders.
        // Observe that final termination within a fixed deadline.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            assert!(
                Instant::now() < deadline,
                "advertiser daemon survived shutdown"
            );
            match browser.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(_) => {}
                Err(mdns_sd::RecvTimeoutError::Disconnected) => break,
                Err(mdns_sd::RecvTimeoutError::Timeout) => {
                    panic!("advertiser daemon survived shutdown")
                }
            }
        }
        assert!(
            finished.load(Ordering::SeqCst),
            "shutdown returned before remaining worker completed"
        );
        assert!(lock(&a.inner.runtime).unwrap().is_none());
        assert!(lock(&a.inner.operations).unwrap().is_empty());
        drop(a);
        let reopened = Session::open(config(ad.path()), keys).unwrap();
        reopened.shutdown().unwrap();
    }
    #[test]
    fn delayed_historical_batch_gets_exact_receipt_without_cursor_regression() {
        let ad = tempfile::tempdir().unwrap();
        let bd = tempfile::tempdir().unwrap();
        let a = Session::open(config(ad.path()), Arc::new(MemoryKeyStore::new())).unwrap();
        let b = Session::open(config(bd.path()), Arc::new(MemoryKeyStore::new())).unwrap();
        pair(&a, &b);
        for sequence in [10, 5] {
            let mut reader = socket(&a, &b);
            let receipt = ManagedReceipt {
                epoch: "b-history".into(),
                sequence,
                proof: vec![0; 32],
            };
            let batch = ExportBatch {
                checkpoint: receipt.clone(),
                records: vec![],
                full: true,
            };
            send(&mut reader, &a, &batch);
            assert_eq!(
                read_message(&mut reader).unwrap(),
                Message::ManagedAck {
                    receipt,
                    disposition: AckDisposition::Stored
                }
            );
            let _ = read_message(&mut reader);
        }
        assert_eq!(
            a.stored_inbound_receipt(&b.snapshot().unwrap().identity.device_id)
                .unwrap()
                .sequence,
            10
        );
    }
    #[test]
    fn second_adapter_validation_failure_cannot_publish_first_adapter_or_receipt() {
        struct Reject(RecordsAdapter);
        impl ManagedAdapter for Reject {
            fn descriptor(&self) -> crate::AdapterDescriptor {
                self.0.descriptor()
            }
            fn prepare(
                &self,
                local: &[ManagedRecord],
                incoming: &[ManagedRecord],
                revision: u64,
            ) -> Result<PreparedChange> {
                if incoming.iter().any(|r| r.value == b"reject") {
                    return fail_code(SessionErrorCode::InvalidRecord, "invalid domain payload");
                }
                self.0.prepare(local, incoming, revision)
            }
        }
        let bd = tempfile::tempdir().unwrap();
        let ad = tempfile::tempdir().unwrap();
        let mut metadata = config(bd.path()).metadata;
        let mut second = metadata.manifest.adapters[0].clone();
        second.id = "inbox".into();
        metadata.manifest.adapters.push(second.clone());
        let mut bc = SessionConfig::new(bd.path(), metadata.clone())
            .with_adapter(Arc::new(Reject(RecordsAdapter::new(second))))
            .unwrap();
        bc.advertise = false;
        bc.listen = SocketAddr::from(([127, 0, 0, 1], 0));
        let mut ac = SessionConfig::new(ad.path(), metadata);
        ac.advertise = false;
        ac.listen = SocketAddr::from(([127, 0, 0, 1], 0));
        let b = Session::open(bc, Arc::new(MemoryKeyStore::new())).unwrap();
        let a = Session::open(ac, Arc::new(MemoryKeyStore::new())).unwrap();
        pair(&b, &a);
        let aid = a.snapshot().unwrap().identity.device_id;
        let mut reader = socket(&b, &a);
        let records = vec![
            ("records", b"valid".to_vec()),
            ("inbox", b"reject".to_vec()),
        ]
        .into_iter()
        .map(|(adapter, value)| ManagedRecord {
            adapter: adapter.into(),
            id: "x".into(),
            value,
            deleted: false,
            clock: LamportClock {
                counter: 1,
                device_id: aid.clone(),
            },
        })
        .collect();
        send(
            &mut reader,
            &b,
            &ExportBatch {
                checkpoint: ManagedReceipt {
                    epoch: "source".into(),
                    sequence: 1,
                    proof: vec![0; 32],
                },
                records,
                full: true,
            },
        );
        assert!(read_message(&mut reader).is_err());
        assert_eq!(b.get("records", "x").unwrap(), None);
        assert_eq!(b.get("inbox", "x").unwrap(), None);
        assert!(b.stored_inbound_receipt(&aid).unwrap().epoch.is_empty());
    }
}
