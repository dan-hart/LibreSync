use super::*;
use crate::{DeviceHandler, PairingInvitation, SecurePairingOutcome, SyncOptions};
use std::{net::SocketAddr, time::Duration};
#[derive(Clone, Serialize, Deserialize)]
pub struct SessionInvitation {
    pub invitation: PairingInvitation,
    pub identity: Identity,
    pub addresses: Vec<SocketAddr>,
}
impl std::fmt::Debug for SessionInvitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionInvitation")
            .field("identity", &self.identity)
            .field("addresses", &self.addresses)
            .finish_non_exhaustive()
    }
}
impl SessionInvitation {
    pub fn encode(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }
    pub fn decode(s: &str) -> Result<Self> {
        if s.len() > 16 * 1024 {
            return fail("invitation too large");
        }
        let invitation: Self = serde_json::from_str(s)?;
        invitation.invitation.validate()?;
        if invitation.addresses.is_empty()
            || invitation.addresses.len() > 32
            || invitation.identity.app_id != invitation.invitation.metadata.manifest.app_id
        {
            return fail("invalid invitation destination");
        }
        Ok(invitation)
    }
}
pub(super) struct Handler {
    pub inner: Arc<Inner>,
    pub generation: u64,
    pub expected: Option<Identity>,
    pub addresses: Vec<SocketAddr>,
    pub recovering: bool,
    pub repair: bool,
}
impl Handler {
    fn check(&self, e: &Envelope) -> Result<()> {
        if e.phase != SessionPhase::Running || e.generation != self.generation {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
    fn validate(&self, e: &Envelope, outcome: &SecurePairingOutcome) -> Result<()> {
        self.check(e)?;
        if self
            .expected
            .as_ref()
            .is_some_and(|id| *id != outcome.identity)
        {
            return fail("authenticated inviter differs from invitation destination");
        }
        if !self
            .inner
            .config
            .metadata
            .manifest
            .compatible_with(&outcome.metadata.manifest)
            || outcome.identity.app_id != e.identity.app_id
            || outcome.identity.device_id == e.identity.device_id
        {
            return fail("incompatible enrollment identity or schema");
        }
        if let Some(peer) = e.peers.get(&outcome.identity.device_id) {
            if (peer.revoked || peer.fingerprint != outcome.fingerprint) && !self.repair {
                return fail("peer revoked or fingerprint changed; explicit repair required");
            }
        }
        Ok(())
    }
}
impl DeviceHandler for Handler {
    fn authenticated_pairing_io_timeout(&self) -> Option<Duration> {
        Some(self.inner.config.authenticated_io_timeout)
    }
    fn allow_legacy_link(&self) -> bool {
        false
    }
    fn app_id(&self) -> &str {
        &self.inner.config.metadata.manifest.app_id
    }
    fn device_metadata(&self) -> Option<DeviceMetadata> {
        Some(self.inner.config.metadata.clone())
    }
    fn pairing_manager(&self) -> Option<Arc<crate::PairingManager>> {
        Some(self.inner.pairing.clone())
    }
    fn device_keys(&self) -> Result<DeviceKeys> {
        Ok(self.inner.keys.clone())
    }
    fn app_key(&self) -> Result<AppKey> {
        AppKey::from_slice(&lock(&self.inner.envelope)?.group_key)
    }
    fn is_linked(&self, id: &Identity) -> bool {
        self.inner.envelope.lock().is_ok_and(|e| {
            self.check(&e).is_ok()
                && e.peers
                    .get(&id.device_id)
                    .is_some_and(|p| !p.revoked && p.identity == *id)
        })
    }
    fn is_linked_with_fingerprint(&self, id: &Identity, fp: &str) -> bool {
        self.inner.envelope.lock().is_ok_and(|e| {
            self.check(&e).is_ok()
                && e.peers
                    .get(&id.device_id)
                    .is_some_and(|p| !p.revoked && p.identity == *id && p.fingerprint == fp)
        })
    }
    fn approve_link(&self, _: &Identity) -> Result<bool> {
        Ok(false)
    }
    fn prepare_secure_pairing(&self, outcome: &SecurePairingOutcome) -> Result<()> {
        self.inner.mutate(|e| {
            self.validate(e, outcome)?;
            e.preparations.insert(
                outcome.invitation_id.clone(),
                Preparation {
                    addresses: self.addresses.clone(),
                    outcome: outcome.clone(),
                    generation: self.generation,
                },
            );
            Ok(())
        })
    }
    fn commit_secure_pairing(
        &self,
        invitation: &str,
        id: &Identity,
        metadata: &DeviceMetadata,
        fp: &str,
        key: &AppKey,
    ) -> Result<()> {
        let outcome = SecurePairingOutcome {
            invitation_id: invitation.into(),
            identity: id.clone(),
            metadata: metadata.clone(),
            fingerprint: fp.into(),
        };
        self.inner.mutate(|e| {
            self.validate(e, &outcome)?;
            if e.group_key != key.as_bytes() && e.peers.values().any(|p| !p.revoked) {
                return fail_code(
                    SessionErrorCode::GroupConflict,
                    "cannot replace transport key of an enrolled group",
                );
            }
            if let Some(p) = e.preparations.get(invitation) {
                if (!self.recovering && p.generation != self.generation) || p.outcome != outcome {
                    return fail("pairing preparation changed");
                }
            }
            if e.group_key != key.as_bytes() {
                for peer in e.peers.values_mut() {
                    peer.incarnation = random_id();
                }
            }
            e.group_key = key.as_bytes().to_vec();
            let fresh = !e
                .enrollments
                .get(invitation)
                .is_some_and(|j| j.outcome == outcome);
            let existed = e.peers.contains_key(&id.device_id);
            e.peers.entry(id.device_id.clone()).or_insert_with(|| Peer {
                identity: id.clone(),
                metadata: metadata.clone(),
                fingerprint: fp.into(),
                addresses: Vec::new(),
                stored: ManagedReceipt::default(),
                applied: ManagedReceipt::default(),
                incarnation: random_id(),
                transmitted: ManagedReceipt::default(),
                received: ManagedReceipt::default(),
                processed: ManagedReceipt::default(),
                bootstrapped: false,
                state: PeerState::Waiting,
                pending: None,
                revoked: false,
            });
            if fresh && (self.repair || existed) {
                e.enrollments
                    .retain(|_, j| j.outcome.identity.device_id != id.device_id);
                let p = e.peers.get_mut(&id.device_id).ok_or(Error::Cancelled)?;
                p.identity = id.clone();
                p.metadata = metadata.clone();
                p.fingerprint = fp.into();
                p.revoked = false;
                p.stored = ManagedReceipt::default();
                p.applied = ManagedReceipt::default();
                p.incarnation = random_id();
                p.transmitted = ManagedReceipt::default();
                p.received = ManagedReceipt::default();
                p.processed = ManagedReceipt::default();
                p.bootstrapped = false;
                p.pending = None;
                p.state = PeerState::Waiting;
            }
            e.enrollments.insert(
                invitation.into(),
                Enrollment {
                    outcome,
                    generation: self.generation,
                },
            );
            e.preparations.remove(invitation);
            self.inner.wake();
            Ok(())
        })
    }
    fn recover_secure_pairing(
        &self,
        invitation: &str,
        id: &Identity,
        fp: &str,
    ) -> Result<Option<DeviceMetadata>> {
        let e = lock(&self.inner.envelope)?;
        self.check(&e)?;
        Ok(e.enrollments
            .get(invitation)
            .filter(|j| {
                j.outcome.identity == *id
                    && j.outcome.fingerprint == fp
                    && e.peers
                        .get(&id.device_id)
                        .is_some_and(|p| !p.revoked && p.fingerprint == fp)
            })
            .map(|j| j.outcome.metadata.clone()))
    }
}
#[cfg(test)]
thread_local! { static INVITATION_BEFORE_MANAGER: std::cell::RefCell<Option<Arc<std::sync::Barrier>>> = const { std::cell::RefCell::new(None) }; }
impl Session {
    pub fn create_invitation(&self, ttl: Duration) -> Result<SessionInvitation> {
        self.invitation(ttl, false)
    }
    pub fn create_code_invitation(&self, ttl: Duration) -> Result<SessionInvitation> {
        self.invitation(ttl, true)
    }
    fn invitation(&self, ttl: Duration, code: bool) -> Result<SessionInvitation> {
        // Serialize invitation publication with start/stop/removal. Never hold
        // the envelope while entering the manager: its authenticated commit
        // callback uses the opposite (manager then envelope) direction.
        let _lifecycle = lock(&self.inner.lifecycle)?;
        let addr = self.address()?;
        let identity = {
            let e = lock(&self.inner.envelope)?;
            if e.phase != SessionPhase::Running {
                return fail("session is not running");
            }
            e.identity.clone()
        };
        #[cfg(test)]
        INVITATION_BEFORE_MANAGER.with(|slot| {
            if let Some(barrier) = slot.borrow_mut().take() {
                barrier.wait();
            }
        });
        let invitation = if code {
            self.inner.pairing.create_code_invitation_with_ttl(
                self.inner.config.metadata.clone(),
                self.inner.keys.fingerprint().to_string(),
                ttl,
            )?
        } else {
            self.inner.pairing.create_invitation(
                self.inner.config.metadata.clone(),
                self.inner.keys.fingerprint().to_string(),
                ttl,
            )?
        };
        let wrapped = SessionInvitation {
            invitation,
            identity,
            addresses: network::addresses(addr)?,
        };
        *lock(&self.inner.invitation)? = Some(wrapped.invitation.descriptor());
        self.inner.refresh_advertisement()?;
        Ok(wrapped)
    }
    /// Blocking Rust operation; SDK callers dispatch this to their IO workers.
    pub fn connect(&self, invitation: &SessionInvitation) -> Result<SecurePairingOutcome> {
        self.connect_inner(invitation, false)
    }
    pub(super) fn connect_inner(
        &self,
        invitation: &SessionInvitation,
        repair: bool,
    ) -> Result<SecurePairingOutcome> {
        invitation.invitation.validate()?;
        if !self
            .inner
            .config
            .metadata
            .manifest
            .compatible_with(&invitation.invitation.metadata.manifest)
        {
            return fail_code(
                SessionErrorCode::IncompatibleSchema,
                "incompatible invitation schema",
            );
        }
        let _pairing = ActivePairing::new(&self.inner, &invitation.invitation.invitation_id)?;
        let operation = OperationGuard::new(&self.inner)?;
        let (identity, generation, cancel) = {
            let e = lock(&self.inner.envelope)?;
            if e.phase != SessionPhase::Running {
                return fail("session is not running");
            }
            let runtime = lock(&self.inner.runtime)?;
            let _runtime = runtime.as_ref().ok_or(Error::Cancelled)?;
            (e.identity.clone(), e.generation, operation.token.clone())
        };
        let handler = Handler {
            inner: self.inner.clone(),
            generation,
            expected: Some(invitation.identity.clone()),
            addresses: invitation.addresses.clone(),
            recovering: false,
            repair,
        };
        let mut last = Error::Protocol("invitation has no endpoints".into());
        for address in &invitation.addresses {
            let options = SyncOptions::new()
                .with_cancel(cancel.clone())
                .with_expected_device_id(&invitation.identity.device_id)
                .with_expected_fingerprint(&invitation.invitation.inviter_fingerprint)
                .with_timeouts(Duration::from_secs(1), Duration::from_secs(3));
            match crate::link_secure(
                &identity,
                &self.inner.config.metadata,
                *address,
                &self.inner.keys,
                &invitation.invitation,
                &handler,
                &options,
            ) {
                Ok(outcome) => {
                    self.inner.mutate(|e| {
                        handler.check(e)?;
                        let p = e
                            .peers
                            .get_mut(&outcome.identity.device_id)
                            .ok_or_else(|| Error::Protocol("enrollment missing".into()))?;
                        p.addresses = invitation.addresses.clone();
                        Ok(())
                    })?;
                    self.inner.wake();
                    return Ok(outcome);
                }
                Err(e) => last = e,
            }
        }
        self.inner
            .diagnostic(Some(invitation.identity.device_id.clone()), &last);
        Err(last)
    }
}

pub(super) fn recover_pending(inner: &Arc<Inner>, cancel: &crate::CancelToken) -> Result<()> {
    let preparations: Vec<_> = lock(&inner.envelope)?
        .preparations
        .values()
        .cloned()
        .collect();
    let active = lock(&inner.active_pairings)?.clone();
    for pending in preparations {
        if active.contains(&pending.outcome.invitation_id) {
            continue;
        }
        cancel.check()?;
        let operation = OperationGuard::new(inner)?;
        let (identity, generation) = {
            let e = lock(&inner.envelope)?;
            (e.identity.clone(), e.generation)
        };
        let handler = Handler {
            inner: inner.clone(),
            generation,
            expected: Some(pending.outcome.identity.clone()),
            addresses: pending.addresses.clone(),
            recovering: true,
            repair: false,
        };
        for address in &pending.addresses {
            let options = SyncOptions::new()
                .with_cancel(operation.token.clone())
                .with_timeouts(Duration::from_millis(300), Duration::from_secs(2));
            match crate::recover_secure_link(
                &identity,
                &inner.config.metadata,
                *address,
                &inner.keys,
                &pending.outcome,
                &handler,
                &options,
            ) {
                Ok(outcome) => {
                    inner.mutate(|e| {
                        handler.check(e)?;
                        let p = e
                            .peers
                            .get_mut(&outcome.identity.device_id)
                            .ok_or(Error::Cancelled)?;
                        p.addresses = pending.addresses.clone();
                        Ok(())
                    })?;
                    break;
                }
                Err(error) => {
                    inner.diagnostic(Some(pending.outcome.identity.device_id.clone()), &error);
                }
            }
        }
    }
    Ok(())
}
impl Session {
    pub fn recover_pairing(&self) -> Result<()> {
        let operation = OperationGuard::new(&self.inner)?;
        recover_pending(&self.inner, &operation.token)
    }
    pub fn pending_pairings(&self) -> Result<Vec<SecurePairingOutcome>> {
        Ok(lock(&self.inner.envelope)?
            .preparations
            .values()
            .map(|p| p.outcome.clone())
            .collect())
    }
}
struct ActivePairing {
    inner: Arc<Inner>,
    id: String,
}
impl ActivePairing {
    fn new(inner: &Arc<Inner>, id: &str) -> Result<Self> {
        if !lock(&inner.active_pairings)?.insert(id.into()) {
            return fail_code(SessionErrorCode::Busy, "pairing operation already running");
        }
        Ok(Self {
            inner: inner.clone(),
            id: id.into(),
        })
    }
}
impl Drop for ActivePairing {
    fn drop(&mut self) {
        if let Ok(mut active) = self.inner.active_pairings.lock() {
            active.remove(&self.id);
        }
        self.inner.wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(path: &std::path::Path) -> SessionConfig {
        let mut c = SessionConfig::new(
            path,
            DeviceMetadata {
                display_name: "test".into(),
                device_kind: "desktop".into(),
                role: "application".into(),
                manifest: crate::AppManifest {
                    app_id: "test.app".into(),
                    display_name: "Test".into(),
                    schema_version: 1,
                    adapters: vec![crate::AdapterDescriptor {
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
    fn handler(s: &Session) -> Handler {
        Handler {
            inner: s.inner.clone(),
            generation: lock(&s.inner.envelope).unwrap().generation,
            expected: None,
            addresses: vec![],
            recovering: false,
            repair: false,
        }
    }
    #[test]
    fn concurrent_invitation_and_authenticated_commit_have_consistent_lock_order() {
        let dir = tempfile::tempdir().unwrap();
        let s = Arc::new(
            Session::open(config(dir.path()), Arc::new(crate::MemoryKeyStore::new())).unwrap(),
        );
        s.start().unwrap();
        let invitation = s.create_invitation(Duration::from_secs(30)).unwrap();
        s.inner
            .pairing
            .claim(&invitation.invitation.invitation_id)
            .unwrap();
        let creation_boundary = Arc::new(std::sync::Barrier::new(2));
        let manager_held = Arc::new(std::sync::Barrier::new(2));
        let commit_session = s.clone();
        let commit_creation = creation_boundary.clone();
        let commit_held = manager_held.clone();
        let commit = std::thread::spawn(move || {
            commit_session
                .inner
                .pairing
                .commit(&invitation.invitation.invitation_id, || {
                    commit_held.wait();
                    commit_creation.wait();
                    // Detect the old opposing lock order without leaving a deadlocked test.
                    let probe = commit_session.inner.envelope.try_lock().map_err(|_| {
                        Error::Protocol("invitation holds envelope while waiting on manager".into())
                    })?;
                    drop(probe);
                    let handler = handler(&commit_session);
                    let identity = Identity::new("new-peer", "test.app", "test-user");
                    handler.commit_secure_pairing(
                        &invitation.invitation.invitation_id,
                        &identity,
                        &commit_session.inner.config.metadata,
                        "aabb",
                        &handler.app_key()?,
                    )
                })
        });
        manager_held.wait();
        let creation_session = s.clone();
        let create = std::thread::spawn(move || {
            INVITATION_BEFORE_MANAGER.with(|slot| *slot.borrow_mut() = Some(creation_boundary));
            creation_session.create_invitation(Duration::from_secs(30))
        });
        let commit_result = commit.join().unwrap();
        let created = create.join().unwrap().unwrap();
        assert!(commit_result.is_ok(), "{commit_result:?}");
        assert!(s.inner.pairing.is_open(&created.invitation.invitation_id));
        assert!(s
            .snapshot()
            .unwrap()
            .peers
            .iter()
            .any(|p| p.identity.device_id == "new-peer"));
        s.shutdown().unwrap();
        assert!(!s.inner.pairing.is_open(&created.invitation.invitation_id));
    }
    #[test]
    fn interrupted_approval_recovers_from_exact_journal_after_client_restart() {
        let ad = tempfile::tempdir().unwrap();
        let bd = tempfile::tempdir().unwrap();
        let bk = Arc::new(crate::MemoryKeyStore::new());
        let a = Session::open(config(ad.path()), Arc::new(crate::MemoryKeyStore::new())).unwrap();
        let b = Session::open(config(bd.path()), bk.clone()).unwrap();
        a.start().unwrap();
        b.start().unwrap();
        let invitation = a.create_invitation(Duration::from_secs(30)).unwrap();
        let outcome = SecurePairingOutcome {
            invitation_id: invitation.invitation.invitation_id.clone(),
            identity: invitation.identity,
            metadata: a.inner.config.metadata.clone(),
            fingerprint: a.inner.keys.fingerprint().into(),
        };
        let mut client = handler(&b);
        client.expected = Some(outcome.identity.clone());
        client.addresses = invitation.addresses.clone();
        client.prepare_secure_pairing(&outcome).unwrap();
        let aid = b.snapshot().unwrap().identity;
        let key = handler(&a).app_key().unwrap();
        handler(&a)
            .commit_secure_pairing(
                &outcome.invitation_id,
                &aid,
                &b.inner.config.metadata,
                b.inner.keys.fingerprint(),
                &key,
            )
            .unwrap();
        b.shutdown().unwrap();
        drop(client);
        drop(b);
        let b = Session::open(config(bd.path()), bk).unwrap();
        b.start().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !b.pending_pairings().unwrap().is_empty() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(b.snapshot().unwrap().peers.len(), 1);
        assert_eq!(handler(&b).app_key().unwrap(), key);
    }
    #[test]
    fn failed_explicit_repair_leaves_revocation_tombstone_and_local_data() {
        let ad = tempfile::tempdir().unwrap();
        let bd = tempfile::tempdir().unwrap();
        let a = Session::open(config(ad.path()), Arc::new(crate::MemoryKeyStore::new())).unwrap();
        let b = Session::open(config(bd.path()), Arc::new(crate::MemoryKeyStore::new())).unwrap();
        a.start().unwrap();
        b.start().unwrap();
        b.connect(&a.create_invitation(Duration::from_secs(30)).unwrap())
            .unwrap();
        b.set("records", "local", b"preserved".to_vec()).unwrap();
        let id = a.snapshot().unwrap().identity.device_id;
        b.remove_peer(&id).unwrap();
        let removed = serde_json::to_value(b.snapshot().unwrap()).unwrap();
        assert_eq!(removed["peers"][0]["revoked"], true);
        assert!(b.resume_peer(&id).is_err());
        let mut invitation = a.create_invitation(Duration::from_secs(30)).unwrap();
        invitation.invitation.secret = "00".repeat(32);
        assert!(b.repair_peer(&id, &invitation).is_err());
        assert_eq!(
            b.get("records", "local").unwrap(),
            Some(b"preserved".to_vec())
        );
        assert!(lock(&b.inner.envelope).unwrap().peers[&id].revoked);
        let public = serde_json::to_value(b.snapshot().unwrap()).unwrap();
        assert_eq!(public["peers"][0]["revoked"], true);
        assert!(b.resume_peer(&id).is_err());

        assert!(b
            .connect(&a.create_invitation(Duration::from_secs(30)).unwrap())
            .is_err());
        b.repair_peer(&id, &a.create_invitation(Duration::from_secs(30)).unwrap())
            .unwrap();
        assert!(!lock(&b.inner.envelope).unwrap().peers[&id].revoked);
        let public = serde_json::to_value(b.snapshot().unwrap()).unwrap();
        assert_eq!(public["peers"][0]["revoked"], false);
        assert_eq!(
            b.get("records", "local").unwrap(),
            Some(b"preserved".to_vec())
        );
    }
}
