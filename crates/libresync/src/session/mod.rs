//! Managed, durable local sessions. The legacy Engine remains an independent API.
mod adapter;
pub(crate) mod compact;
mod limits;
mod receipt;
mod store;
pub use adapter::{AdapterInspection, ManagedAdapter, PreparedChange, RecordsAdapter};
mod network;
mod pairing;
use crate::{AppKey, DeviceKeys, DeviceMetadata, Error, Identity, KeyStore, LamportClock, Result};
pub use pairing::SessionInvitation;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct SessionConfig {
    pub state_dir: PathBuf,
    pub metadata: DeviceMetadata,
    pub listen: std::net::SocketAddr,
    pub advertise: bool,
    pub catch_up: std::time::Duration,
    pub debounce: std::time::Duration,
    /// Idle I/O budget after an enrolled peer is authenticated (maximum five minutes).
    pub authenticated_io_timeout: std::time::Duration,
    /// Maximum encrypted pre-merge recovery copies. Live records are not pruned.
    pub recovery_retention: usize,
    adapters: BTreeMap<String, Arc<dyn ManagedAdapter>>,
}
impl SessionConfig {
    pub fn new(path: impl Into<PathBuf>, metadata: DeviceMetadata) -> Self {
        let adapters = metadata
            .manifest
            .adapters
            .iter()
            .map(|d| {
                (
                    d.id.clone(),
                    Arc::new(RecordsAdapter::new(d.clone())) as Arc<dyn ManagedAdapter>,
                )
            })
            .collect();
        Self {
            state_dir: path.into(),
            metadata,
            listen: "0.0.0.0:0"
                .parse()
                .unwrap_or_else(|_| std::net::SocketAddr::from(([0, 0, 0, 0], 0))),
            advertise: true,
            catch_up: std::time::Duration::from_secs(2),
            debounce: std::time::Duration::from_millis(50),
            authenticated_io_timeout: std::time::Duration::from_secs(60),
            recovery_retention: 20,
            adapters,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManagedRecord {
    pub adapter: String,
    pub id: String,
    #[serde(with = "compact::record_bytes")]
    pub value: Vec<u8>,
    pub deleted: bool,
    pub clock: LamportClock,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionPhase {
    Stopped,
    Failed,
    Running,
    Paused,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub identity: Identity,
    pub phase: SessionPhase,
    pub local_revision: u64,
    pub peers: Vec<PeerSnapshot>,
    pub diagnostics: Vec<SessionEvent>,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Envelope {
    version: u32,
    prepared: Option<PreparedTransaction>,
    recovery: Vec<RecoverySnapshot>,
    identity: Identity,
    metadata: DeviceMetadata,
    group_key: Vec<u8>,
    epoch: String,
    revision: u64,
    local_revision: u64,
    sequences: BTreeMap<String, u64>,
    origins: BTreeMap<String, String>,
    peers: BTreeMap<String, Peer>,
    enrollments: BTreeMap<String, Enrollment>,
    preparations: BTreeMap<String, Preparation>,
    phase: SessionPhase,
    generation: u64,
    records: BTreeMap<String, ManagedRecord>,
}
pub struct Session {
    inner: Arc<Inner>,
}
struct Inner {
    storage_uncertain: std::sync::atomic::AtomicBool,
    wake: (flume::Sender<()>, flume::Receiver<()>),
    _lease: std::fs::File,
    config: SessionConfig,
    keys: DeviceKeys,
    storage_key: AppKey,
    envelope: Mutex<Envelope>,
    runtime: Mutex<Option<network::Runtime>>,
    lifecycle: Mutex<()>,
    pairing: Arc<crate::PairingManager>,
    events: Mutex<Vec<flume::Sender<SessionEvent>>>,
    diagnostics: Mutex<Vec<SessionEvent>>,
    operations: Mutex<BTreeMap<String, crate::CancelToken>>,
    discovered: Mutex<BTreeMap<String, crate::DiscoveredPeer>>,
    invitation: Mutex<Option<crate::PairingDescriptor>>,
    active_pairings: Mutex<std::collections::BTreeSet<String>>,
}
impl Session {
    pub fn open(mut config: SessionConfig, keystore: Arc<dyn KeyStore>) -> Result<Self> {
        config.metadata.validate()?;
        if config.adapters.len() != config.metadata.manifest.adapters.len()
            || config.metadata.manifest.adapters.iter().any(|d| {
                config
                    .adapters
                    .get(&d.id)
                    .is_none_or(|a| a.descriptor() != *d)
            })
        {
            return fail_code(
                SessionErrorCode::InvalidConfiguration,
                "registered adapters differ from manifest",
            );
        }
        if !(1..=100).contains(&config.recovery_retention)
            || config.catch_up.is_zero()
            || config.debounce > std::time::Duration::from_secs(5)
            || config.authenticated_io_timeout.is_zero()
            || config.authenticated_io_timeout > std::time::Duration::from_secs(300)
        {
            return fail_code(
                SessionErrorCode::InvalidConfiguration,
                "invalid managed scheduling intervals",
            );
        }

        std::fs::create_dir_all(&config.state_dir)?;
        config.state_dir = std::fs::canonicalize(&config.state_dir)?;
        let lease = store::lease(&config)?;
        if config
            .metadata
            .manifest
            .adapters
            .iter()
            .any(|a| !a.transactional)
        {
            return fail("managed adapter must support transactions");
        }
        let (identity, keys, storage_key) = store::identity(&config, keystore.as_ref())?;
        let mut envelope = match store::load(&config, &storage_key)? {
            Some(e) => {
                if e.identity != identity
                    || !e
                        .metadata
                        .manifest
                        .compatible_with(&config.metadata.manifest)
                {
                    return fail("session identity or manifest changed");
                }
                e
            }
            None => Envelope {
                version: 1,
                prepared: None,
                recovery: Vec::new(),
                identity,
                metadata: config.metadata.clone(),
                group_key: AppKey::generate()?.as_bytes().to_vec(),
                epoch: random_id(),
                revision: 0,
                local_revision: 0,
                sequences: BTreeMap::new(),
                origins: BTreeMap::new(),
                peers: BTreeMap::new(),
                enrollments: BTreeMap::new(),
                preparations: BTreeMap::new(),
                phase: SessionPhase::Stopped,
                generation: 0,
                records: BTreeMap::new(),
            },
        };
        if let Some(prepared) = envelope.prepared.clone() {
            recover_transaction(&config, &envelope, &prepared)?;
            envelope = *prepared.target;
        }
        limits::state(&envelope)?;
        envelope.phase = SessionPhase::Stopped;
        envelope.generation = envelope
            .generation
            .checked_add(1)
            .ok_or_else(|| Error::Protocol("generation exhausted".into()))?;
        store::save(&config, &storage_key, &envelope)?;
        Ok(Self {
            inner: Arc::new(Inner {
                storage_uncertain: std::sync::atomic::AtomicBool::new(false),
                wake: flume::bounded(1),
                _lease: lease,
                config,
                keys,
                storage_key,
                envelope: Mutex::new(envelope),
                runtime: Mutex::new(None),
                lifecycle: Mutex::new(()),
                pairing: Arc::new(crate::PairingManager::new()),
                events: Mutex::new(Vec::new()),
                diagnostics: Mutex::new(Vec::new()),
                operations: Mutex::new(BTreeMap::new()),
                discovered: Mutex::new(BTreeMap::new()),
                invitation: Mutex::new(None),
                active_pairings: Mutex::new(std::collections::BTreeSet::new()),
            }),
        })
    }
    pub fn snapshot(&self) -> Result<SessionSnapshot> {
        let e = lock(&self.inner.envelope)?;
        Ok(SessionSnapshot {
            identity: e.identity.clone(),
            phase: if self
                .inner
                .storage_uncertain
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                SessionPhase::Failed
            } else {
                e.phase.clone()
            },
            local_revision: e.local_revision,
            diagnostics: lock(&self.inner.diagnostics)?.clone(),
            peers: e
                .peers
                .values()
                .map(|p| {
                    let count = pending(&self.inner, &e, p)?;
                    Ok(PeerSnapshot {
                        identity: p.identity.clone(),
                        metadata: p.metadata.clone(),
                        fingerprint: p.fingerprint.clone(),
                        revoked: p.revoked,
                        pending: count,
                        stored: p.stored.clone(),
                        applied: p.applied.clone(),
                        transmitted: p.transmitted.clone(),
                        received: p.received.clone(),
                        processed: p.processed.clone(),
                        state: if p.revoked
                            || e.phase != SessionPhase::Running
                            || self
                                .inner
                                .storage_uncertain
                                .load(std::sync::atomic::Ordering::SeqCst)
                        {
                            PeerState::Paused
                        } else if p.pending.is_some() {
                            PeerState::NeedsMerge
                        } else if count > 0 && p.state == PeerState::UpToDate {
                            PeerState::Waiting
                        } else {
                            p.state.clone()
                        },
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        })
    }
    pub fn set(&self, adapter: &str, id: &str, value: Vec<u8>) -> Result<()> {
        self.write(adapter, id, value, false)
    }
    pub fn delete(&self, adapter: &str, id: &str) -> Result<()> {
        self.write(adapter, id, Vec::new(), true)
    }
    fn write(&self, adapter: &str, id: &str, value: Vec<u8>, deleted: bool) -> Result<()> {
        self.check_adapter(adapter)?;
        if id.is_empty() || id.len() > 1024 || value.len() > compact::MAX_RECORD_BYTES {
            return fail_code(
                SessionErrorCode::InvalidRecord,
                "invalid managed record size",
            );
        }
        let result = self.inner.mutate(|e| {
            let counter = e
                .records
                .values()
                .map(|r| r.clock.counter)
                .max()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| Error::Protocol("clock exhausted".into()))?;
            let incoming = ManagedRecord {
                adapter: adapter.into(),
                id: id.into(),
                value,
                deleted,
                clock: LamportClock {
                    counter,
                    device_id: e.identity.device_id.clone(),
                },
            };
            let prepared = self.inner.prepare_records(e, &[incoming])?;
            self.inner.commit_records(e, &prepared, None)?;
            e.local_revision = e
                .local_revision
                .checked_add(1)
                .ok_or_else(|| Error::Protocol("local revision exhausted".into()))?;
            Ok(())
        });
        if result.is_ok() {
            self.inner.wake();
        }
        result
    }
    pub fn get(&self, adapter: &str, id: &str) -> Result<Option<Vec<u8>>> {
        self.check_adapter(adapter)?;
        Ok(lock(&self.inner.envelope)?
            .records
            .get(&record_key(adapter, id)?)
            .filter(|r| !r.deleted)
            .map(|r| r.value.clone()))
    }
    pub fn records(&self, adapter: &str) -> Result<Vec<ManagedRecord>> {
        self.check_adapter(adapter)?;
        Ok(lock(&self.inner.envelope)?
            .records
            .values()
            .filter(|r| r.adapter == adapter)
            .cloned()
            .collect())
    }
    fn check_adapter(&self, adapter: &str) -> Result<()> {
        if self
            .inner
            .config
            .metadata
            .manifest
            .adapters
            .iter()
            .any(|a| a.id == adapter)
        {
            Ok(())
        } else {
            fail("unknown managed adapter")
        }
    }
}
#[cfg(test)]
thread_local! { static MUTATION_BEFORE_LOCK: std::cell::RefCell<Option<Arc<std::sync::Barrier>>> = const { std::cell::RefCell::new(None) }; }
impl Inner {
    fn mutate<R>(&self, operation: impl FnOnce(&mut Envelope) -> Result<R>) -> Result<R> {
        if self
            .storage_uncertain
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return fail_code(
                SessionErrorCode::StorageCommitUncertain,
                "storage commit outcome uncertain; close and reopen session for recovery",
            );
        }
        #[cfg(test)]
        MUTATION_BEFORE_LOCK.with(|slot| {
            if let Some(barrier) = slot.borrow_mut().take() {
                barrier.wait();
            }
        });
        let mut current = lock(&self.envelope)?;
        // A preceding writer can fail while this mutation waits for the lock.
        // Refuse recovery and application work once that writer fences storage.
        if self
            .storage_uncertain
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return fail_code(
                SessionErrorCode::StorageCommitUncertain,
                "storage commit outcome uncertain; close and reopen session for recovery",
            );
        }
        if let Some(prepared) = current.prepared.clone() {
            recover_transaction(&self.config, &current, &prepared)?;
            self.save_candidate(&prepared.target)?;
            *current = *prepared.target;
        }
        let mut candidate = current.clone();
        let result = operation(&mut candidate)?;
        if candidate == *current {
            return Ok(result);
        }
        if candidate.records != current.records {
            limits::state(&candidate)?;
            let id = random_id();
            if current.peers.values().any(|p| {
                !p.bootstrapped
                    && candidate
                        .peers
                        .get(&p.identity.device_id)
                        .is_some_and(|next| next.bootstrapped)
            }) && !current.records.is_empty()
            {
                candidate.recovery.push(RecoverySnapshot {
                    id: id.clone(),
                    revision: current.revision,
                    records: current.records.values().cloned().collect(),
                });
            }
            let remove = candidate
                .recovery
                .len()
                .saturating_sub(self.config.recovery_retention);
            candidate.recovery.drain(..remove);
            let mut changes = Vec::new();
            for adapter in self.config.adapters.keys() {
                let before: Vec<_> = current
                    .records
                    .values()
                    .filter(|r| &r.adapter == adapter)
                    .cloned()
                    .collect();
                let after: Vec<_> = candidate
                    .records
                    .values()
                    .filter(|r| &r.adapter == adapter)
                    .cloned()
                    .collect();
                if before != after {
                    changes.push(PreparedChange {
                        id: id.clone(),
                        adapter: adapter.clone(),
                        expected_revision: current.revision,
                        staged: after,
                        recovery: before,
                    });
                }
            }
            let prepared = PreparedTransaction {
                id,
                changes,
                target: Box::new(candidate.clone()),
            };
            let mut journal = current.clone();
            journal.prepared = Some(prepared);
            self.save_candidate(&journal)?;
            *current = journal;
        }
        self.save_candidate(&candidate)?;
        *current = candidate;
        self.emit(SessionEvent::Changed);
        Ok(result)
    }
}
fn lock<T>(mutex: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>> {
    mutex
        .lock()
        .map_err(|_| Error::Protocol("managed session lock poisoned".into()))
}
fn fail<T>(message: &str) -> Result<T> {
    fail_code(SessionErrorCode::InvalidOperation, message)
}
fn random_id() -> String {
    use rand_core::{OsRng, RngCore};
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn record_key(adapter: &str, id: &str) -> Result<String> {
    Ok(serde_json::to_string(&(adapter, id))?)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ManagedReceipt {
    pub epoch: String,
    pub sequence: u64,
    /// Opaque source-issued evidence; preserve verbatim with this checkpoint.
    #[serde(default, with = "compact::proof_bytes")]
    pub proof: Vec<u8>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum AckDisposition {
    Stored,
    Applied,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum PeerState {
    Waiting,
    Exchanging,
    Stored,
    UpToDate,
    NeedsMerge,
    NeedsRepair,
    Paused,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerSnapshot {
    pub identity: Identity,
    pub metadata: DeviceMetadata,
    pub fingerprint: String,
    /// Removed trust tombstone. A fresh invitation and explicit repair are required.
    #[serde(default)]
    pub revoked: bool,
    pub pending: usize,
    pub stored: ManagedReceipt,
    pub applied: ManagedReceipt,
    pub transmitted: ManagedReceipt,
    pub received: ManagedReceipt,
    pub processed: ManagedReceipt,
    pub state: PeerState,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SessionEvent {
    Changed,
    Diagnostic {
        peer: Option<String>,
        message: String,
        action: DiagnosticAction,
        evidence: DiagnosticEvidence,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum DiagnosticAction {
    None,
    CheckPermissions,
    Retry,
    RepairPeer,
    ReviewMerge,
    GrantPermission,
    CheckSecureStorage,
    ReviewCompatibility,
    ReviewRecords,
    ResolveGroupConflict,
    ReviewConfiguration,
    Wait,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PermissionState {
    Allowed,
    Denied,
    Unknown,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DiagnosticEvidence {
    Unknown,
    ManagedFailure {
        code: SessionErrorCode,
    },
    StorageCommitUncertain,
    NetworkError,
    CertificateMismatch,
    Platform {
        platform: String,
        permission: String,
        state: PermissionState,
    },
    BootstrapRequired,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Peer {
    identity: Identity,
    metadata: DeviceMetadata,
    fingerprint: String,
    addresses: Vec<std::net::SocketAddr>,
    stored: ManagedReceipt,
    applied: ManagedReceipt,
    #[serde(default = "random_id")]
    incarnation: String,
    transmitted: ManagedReceipt,
    received: ManagedReceipt,
    processed: ManagedReceipt,
    bootstrapped: bool,
    state: PeerState,
    pending: Option<BootstrapPreview>,
    revoked: bool,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Enrollment {
    outcome: crate::SecurePairingOutcome,
    generation: u64,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Preparation {
    addresses: Vec<std::net::SocketAddr>,
    outcome: crate::SecurePairingOutcome,
    generation: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExportBatch {
    pub checkpoint: ManagedReceipt,
    pub records: Vec<ManagedRecord>,
    pub full: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BootstrapPreview {
    pub token: String,
    pub peer: String,
    pub fingerprint: String,
    pub local_revision: u64,
    pub batch: ExportBatch,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum BootstrapDecision {
    Merge,
    Cancel,
}
fn pending(inner: &Inner, e: &Envelope, p: &Peer) -> Result<usize> {
    let records: Vec<_> = e
        .records
        .iter()
        .filter(|(k, _)| {
            (p.stored.epoch != e.epoch || p.stored.sequence > e.revision)
                || (e.sequences.get(*k).copied().unwrap_or(u64::MAX) > p.stored.sequence
                    && e.origins.get(*k) != Some(&p.identity.device_id))
        })
        .map(|(_, r)| r.clone())
        .collect();
    Ok(inner.export_records(&records)?.len())
}

impl Inner {
    fn emit(&self, event: SessionEvent) {
        if matches!(event, SessionEvent::Diagnostic { .. }) {
            if let Ok(mut diagnostics) = self.diagnostics.lock() {
                if diagnostics.len() >= 128 {
                    diagnostics.remove(0);
                }
                diagnostics.push(event.clone());
            }
        }
        if let Ok(mut subscribers) = self.events.lock() {
            subscribers.retain(|s| {
                !matches!(
                    s.try_send(event.clone()),
                    Err(flume::TrySendError::Disconnected(_))
                )
            });
        }
    }
    fn diagnostic(&self, peer: Option<String>, error: &Error) {
        let (action, evidence) = if self
            .storage_uncertain
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            (
                DiagnosticAction::CheckSecureStorage,
                DiagnosticEvidence::StorageCommitUncertain,
            )
        } else {
            match error {
                Error::FingerprintMismatch { .. } => (
                    DiagnosticAction::RepairPeer,
                    DiagnosticEvidence::CertificateMismatch,
                ),
                Error::Managed {
                    code: SessionErrorCode::StorageCommitUncertain,
                    ..
                } => (
                    DiagnosticAction::CheckSecureStorage,
                    DiagnosticEvidence::StorageCommitUncertain,
                ),
                Error::Managed { code, .. } => {
                    let action = match code {
                        SessionErrorCode::StorageUnavailable => {
                            DiagnosticAction::CheckSecureStorage
                        }
                        SessionErrorCode::IncompatibleSchema => {
                            DiagnosticAction::ReviewCompatibility
                        }
                        SessionErrorCode::InvalidRecord => DiagnosticAction::ReviewRecords,
                        SessionErrorCode::InvalidConfiguration
                        | SessionErrorCode::InvalidAdapter
                        | SessionErrorCode::InvalidOperation => {
                            DiagnosticAction::ReviewConfiguration
                        }
                        SessionErrorCode::GroupConflict => DiagnosticAction::ResolveGroupConflict,
                        SessionErrorCode::StaleBootstrap => DiagnosticAction::ReviewMerge,
                        SessionErrorCode::PeerRevoked => DiagnosticAction::RepairPeer,
                        SessionErrorCode::Busy => DiagnosticAction::Wait,
                        SessionErrorCode::StorageCommitUncertain => {
                            DiagnosticAction::CheckSecureStorage
                        }
                    };
                    (
                        action,
                        DiagnosticEvidence::ManagedFailure { code: code.clone() },
                    )
                }
                Error::Cancelled => (DiagnosticAction::None, DiagnosticEvidence::Unknown),
                _ => (DiagnosticAction::Retry, DiagnosticEvidence::Unknown),
            }
        };
        self.emit(SessionEvent::Diagnostic {
            peer,
            message: error.to_string(),
            action,
            evidence,
        });
    }
}
impl Session {
    pub fn subscribe(&self) -> Result<flume::Receiver<SessionEvent>> {
        let (s, r) = flume::bounded(256);
        let mut events = lock(&self.inner.events)?;
        events.retain(|sender| !sender.is_disconnected());
        events.push(s);
        Ok(r)
    }
    pub fn report_platform_evidence(
        &self,
        platform: String,
        permission: String,
        state: PermissionState,
    ) {
        self.inner.emit(SessionEvent::Diagnostic {
            peer: None,
            message: format!("{permission}: {state:?}"),
            action: match state {
                PermissionState::Allowed => DiagnosticAction::None,
                PermissionState::Denied => DiagnosticAction::GrantPermission,
                PermissionState::Unknown => DiagnosticAction::CheckPermissions,
            },
            evidence: DiagnosticEvidence::Platform {
                platform,
                permission,
                state,
            },
        });
    }
    pub fn notify_local_change(&self) -> Result<()> {
        self.inner.wake();
        self.inner.emit(SessionEvent::Changed);
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if let Err(e) = self.shutdown() {
            self.inner.diagnostic(None, &e);
        }
    }
}

struct OperationGuard {
    inner: Arc<Inner>,
    id: String,
    token: crate::CancelToken,
}
impl OperationGuard {
    fn preauthenticated(
        inner: &Arc<Inner>,
        cancel: &crate::CancelToken,
        socket: &std::net::TcpStream,
    ) -> Result<Self> {
        let mut operations = lock(&inner.operations)?;
        cancel.check()?;
        let id = random_id();
        let token = crate::CancelToken::new();
        token.register(socket)?;
        operations.insert(id.clone(), token.clone());
        Ok(Self {
            inner: inner.clone(),
            id,
            token,
        })
    }
    fn new(inner: &Arc<Inner>) -> Result<Self> {
        let e = lock(&inner.envelope)?;
        if e.phase != SessionPhase::Running {
            return Err(Error::Cancelled);
        }
        let id = random_id();
        let token = crate::CancelToken::new();
        lock(&inner.operations)?.insert(id.clone(), token.clone());
        Ok(Self {
            inner: inner.clone(),
            id,
            token,
        })
    }
}
impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.token.release();
        if let Ok(mut operations) = self.inner.operations.lock() {
            operations.remove(&self.id);
        }
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PreparedTransaction {
    id: String,
    changes: Vec<PreparedChange>,
    target: Box<Envelope>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecoverySnapshot {
    pub id: String,
    pub revision: u64,
    pub records: Vec<ManagedRecord>,
}
fn recover_transaction(
    config: &SessionConfig,
    current: &Envelope,
    prepared: &PreparedTransaction,
) -> Result<()> {
    limits::state(&prepared.target)?;
    if prepared.target.prepared.is_some()
        || prepared.target.identity != current.identity
        || prepared.id.is_empty()
    {
        return fail("invalid prepared transaction journal");
    }
    for change in &prepared.changes {
        if change.expected_revision != current.revision {
            return fail("recovery revision mismatch");
        }
        let adapter = config
            .adapters
            .get(&change.adapter)
            .ok_or_else(|| Error::Protocol("recovery adapter absent".into()))?;
        let recovered = adapter.recover(change)?;
        let expected: Vec<_> = prepared
            .target
            .records
            .values()
            .filter(|r| r.adapter == change.adapter)
            .cloned()
            .collect();
        if recovered != expected {
            return fail("adapter recovery did not reproduce committed candidate");
        }
    }
    Ok(())
}
impl Session {
    pub fn recovery_snapshots(&self) -> Result<Vec<RecoverySnapshot>> {
        Ok(lock(&self.inner.envelope)?.recovery.clone())
    }
}

impl Inner {
    fn save_candidate(&self, envelope: &Envelope) -> Result<()> {
        match store::save(&self.config, &self.storage_key, envelope) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.storage_uncertain
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                self.emit(SessionEvent::Diagnostic {
                    peer: None,
                    message: format!(
                        "Storage commit outcome uncertain: {error}; close and reopen for recovery"
                    ),
                    action: DiagnosticAction::CheckSecureStorage,
                    evidence: DiagnosticEvidence::StorageCommitUncertain,
                });
                Err(error)
            }
        }
    }
}

impl Inner {
    fn wake(&self) {
        let _ = self.wake.0.try_send(());
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApplicationInbox {
    pub revision: u64,
    pub records: Vec<ManagedRecord>,
    pub receipts: BTreeMap<String, ManagedReceipt>,
}
impl Session {
    /// Atomically captures records and the exact cumulative source receipts
    /// represented by them. Apply this snapshot to the app, then acknowledge it.
    pub fn application_inbox(&self) -> Result<ApplicationInbox> {
        let e = lock(&self.inner.envelope)?;
        Ok(ApplicationInbox {
            revision: e.revision,
            records: e.records.values().cloned().collect(),
            receipts: e
                .peers
                .iter()
                .filter(|(_, p)| !p.revoked && !p.received.epoch.is_empty())
                .map(|(id, p)| (id.clone(), p.received.clone()))
                .collect(),
        })
    }
    pub fn acknowledge_inbox(&self, inbox: &ApplicationInbox) -> Result<()> {
        self.inner.mutate(|e| {
            if inbox.revision > e.revision {
                return fail("application snapshot revision is in the future");
            }
            for (id, receipt) in &inbox.receipts {
                let p = e.peers.get(id).ok_or(Error::Cancelled)?;
                if p.revoked
                    || p.received.epoch != receipt.epoch
                    || receipt.sequence > p.received.sequence
                {
                    return fail("application receipt is outside stored history");
                }
            }
            for (id, receipt) in &inbox.receipts {
                let p = e.peers.get_mut(id).ok_or(Error::Cancelled)?;
                if p.processed.epoch != receipt.epoch || receipt.sequence > p.processed.sequence {
                    p.processed = receipt.clone();
                }
            }
            Ok(())
        })?;
        self.inner.wake();
        Ok(())
    }
    pub fn wake(&self) -> Result<()> {
        self.notify_local_change()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionErrorCode {
    InvalidOperation,
    InvalidConfiguration,
    InvalidAdapter,
    InvalidRecord,
    IncompatibleSchema,
    StaleBootstrap,
    GroupConflict,
    PeerRevoked,
    StorageUnavailable,
    StorageCommitUncertain,
    Busy,
}
fn fail_code<T>(code: SessionErrorCode, message: &str) -> Result<T> {
    Err(Error::Managed {
        code,
        message: message.into(),
    })
}

impl Session {
    /// Import an app-produced logical batch, preserving original clocks and
    /// tombstones. Validation and durable publication are atomic. This is a
    /// local app write, not a bootstrap consent bypass or Applied receipt.
    pub fn import_records(&self, records: &[ManagedRecord]) -> Result<()> {
        for record in records {
            self.check_adapter(&record.adapter)?;
        }
        limits::batch(&ExportBatch {
            checkpoint: ManagedReceipt::default(),
            records: records.to_vec(),
            full: false,
        })?;
        self.inner.mutate(|e| {
            let prepared = self.inner.prepare_records(e, records)?;
            let before = e.revision;
            self.inner.commit_records(e, &prepared, None)?;
            if e.revision != before {
                e.local_revision = e
                    .local_revision
                    .checked_add(1)
                    .ok_or_else(|| Error::Protocol("local revision exhausted".into()))?;
            }
            Ok(())
        })?;
        self.inner.wake();
        Ok(())
    }
    /// Metadata-only recovery inspection for UI polling. Tuples contain
    /// (snapshot ID, source revision, record count); payloads are not cloned.
    pub fn recovery_snapshot_sizes(&self) -> Result<Vec<(String, u64, usize)>> {
        Ok(lock(&self.inner.envelope)?
            .recovery
            .iter()
            .map(|snapshot| {
                (
                    snapshot.id.clone(),
                    snapshot.revision,
                    snapshot.records.len(),
                )
            })
            .collect())
    }
    /// Prune encrypted pre-merge recovery copies only; live records and their
    /// tombstones are unaffected. The caller declares the retention limit.
    pub fn prune_recovery_snapshots(&self, retain: usize) -> Result<()> {
        self.inner.mutate(|e| {
            let remove = e.recovery.len().saturating_sub(retain);
            e.recovery.drain(..remove);
            Ok(())
        })
    }
}

/// Public platform advertisement. Contains no enrollment secret or trust grant.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlatformAdvertisement {
    pub service_type: String,
    pub instance: String,
    pub port: u16,
    pub ipv6: bool,
    pub txt: std::collections::HashMap<String, String>,
}
impl Session {
    /// Platform Bonjour uses the actual Rust listener; never creates a second listener.
    pub fn platform_advertisement(&self) -> Result<PlatformAdvertisement> {
        let address = self.address()?;
        let identity = lock(&self.inner.envelope)?.identity.clone();
        let invitation = lock(&self.inner.invitation)?
            .clone()
            .filter(|d| self.inner.pairing.is_open(&d.invitation_id));
        Ok(PlatformAdvertisement {
            service_type: "_libresync._tcp.".into(),
            instance: identity.device_id.clone(),
            port: address.port(),
            ipv6: address.is_ipv6(),
            txt: crate::discovery::platform_properties(
                &identity,
                &self.inner.config.metadata,
                invitation.as_ref(),
            )?,
        })
    }
    /// Bounded, untrusted endpoint hints. TLS pinning and authenticated schema
    /// checks still govern every connection. IPv4 listeners discard AAAA hints.
    pub fn ingest_platform_discovery(&self, mut peer: crate::DiscoveredPeer) -> Result<()> {
        let own = lock(&self.inner.envelope)?.identity.clone();
        if peer.identity == own {
            return Ok(());
        }
        let bounded = |s: &str| !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control);
        if peer.identity.app_id != own.app_id
            || !bounded(&peer.identity.device_id)
            || !bounded(&peer.identity.user_id)
            || peer.addresses.is_empty()
            || peer.addresses.len() > 32
        {
            return fail_code(
                SessionErrorCode::InvalidConfiguration,
                "invalid platform discovery hint",
            );
        }
        let ad = peer.advertisement.as_ref().ok_or_else(|| Error::Managed {
            code: SessionErrorCode::InvalidConfiguration,
            message: "missing platform metadata".into(),
        })?;
        if !bounded(&ad.display_name)
            || !bounded(&ad.device_kind)
            || !bounded(&ad.role)
            || !bounded(&ad.app_display_name)
            || ad.contract_digest.len() != 64
            || !ad.contract_digest.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return fail_code(
                SessionErrorCode::InvalidConfiguration,
                "invalid platform metadata",
            );
        }
        if let Some(d) = &peer.invitation {
            if d.invitation_id.len() != 32
                || !d.invitation_id.bytes().all(|b| b.is_ascii_hexdigit())
                || d.inviter_fingerprint.len() != 64
                || !d.inviter_fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return fail_code(
                    SessionErrorCode::InvalidConfiguration,
                    "invalid pairing hints",
                );
            }
        }
        let ipv6 = self.address()?.is_ipv6();
        peer.addresses.retain(|a| {
            a.port() != 0
                && !a.ip().is_unspecified()
                && !a.ip().is_multicast()
                && (ipv6 || a.is_ipv4())
        });
        peer.addresses.sort();
        peer.addresses.dedup();
        if peer.addresses.is_empty() {
            return Ok(());
        }
        {
            let mut discovered = lock(&self.inner.discovered)?;
            if discovered.len() >= 256 && !discovered.contains_key(&peer.identity.device_id) {
                return fail_code(
                    SessionErrorCode::Busy,
                    "platform discovery capacity reached",
                );
            }
            if discovered.get(&peer.identity.device_id) == Some(&peer) {
                return Ok(());
            }
            discovered.insert(peer.identity.device_id.clone(), peer.clone());
        }
        self.inner.emit(SessionEvent::Changed);
        self.inner.wake();
        Ok(())
    }
    /// Withdraw a browse result; enrollment and stored records remain intact.
    pub fn withdraw_platform_discovery(&self, device_id: &str) -> Result<()> {
        lock(&self.inner.discovered)?.remove(device_id);
        self.inner.emit(SessionEvent::Changed);
        Ok(())
    }
    /// Cancel current network operations while retaining enrollment and data.
    /// Future wake/scheduler cycles may retry; use pause for sustained suspension.
    pub fn cancel_operations(&self) -> Result<()> {
        for token in lock(&self.inner.operations)?.values() {
            token.cancel();
        }
        Ok(())
    }
}

impl Inner {
    fn endpoint_hints(
        &self,
        identity: &Identity,
        fallback: &[std::net::SocketAddr],
    ) -> Result<Vec<std::net::SocketAddr>> {
        Ok(lock(&self.discovered)?
            .get(&identity.device_id)
            .filter(|p| p.identity == *identity && !p.addresses.is_empty())
            .map(|p| p.addresses.clone())
            .unwrap_or_else(|| fallback.to_vec()))
    }
}

#[cfg(test)]
mod subscription_tests {
    use super::*;
    #[test]
    fn idle_subscription_churn_is_bounded_and_preserves_live_observers() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = SessionConfig::new(
            dir.path(),
            DeviceMetadata {
                display_name: "Observers".into(),
                device_kind: "desktop".into(),
                role: "device".into(),
                manifest: crate::companion::notes_manifest(),
            },
        );
        config.advertise = false;
        let session = Session::open(config, Arc::new(crate::MemoryKeyStore::new())).unwrap();
        let first = session.subscribe().unwrap();
        for _ in 0..512 {
            drop(session.subscribe().unwrap());
        }
        assert!(
            lock(&session.inner.events).unwrap().len() <= 2,
            "Idle dropped queues must be pruned without waiting for an event"
        );
        let second = session.subscribe().unwrap();
        session
            .set("records", "observed", b"change".to_vec())
            .unwrap();
        assert!(matches!(
            first.recv_timeout(std::time::Duration::from_secs(1)),
            Ok(SessionEvent::Changed)
        ));
        assert!(matches!(
            second.recv_timeout(std::time::Duration::from_secs(1)),
            Ok(SessionEvent::Changed)
        ));
    }
}
