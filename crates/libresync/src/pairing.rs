//! Authenticated, expiring enrollment. Advertisements are never trust evidence.
use crate::{Error, Identity, Result};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const PAIRING_VERSION: u32 = 2;
pub const MAX_PAIRING_BYTES: u64 = 16 * 1024;
const MAX_TTL: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdapterDescriptor {
    pub id: String,
    pub namespace: String,
    pub schema: String,
    pub transactional: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppManifest {
    pub app_id: String,
    pub display_name: String,
    pub schema_version: u32,
    pub adapters: Vec<AdapterDescriptor>,
}
impl AppManifest {
    /// Hint for discovery only; authenticated full manifests remain mandatory.
    pub fn contract_digest(&self) -> Result<String> {
        self.validate()?;
        let mut adapters = self.adapters.clone();
        adapters.sort_by(|a, b| a.id.cmp(&b.id));
        let bytes = serde_json::to_vec(&(&self.app_id, self.schema_version, adapters))?;
        Ok(Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect())
    }
    pub fn compatible_with(&self, other: &Self) -> bool {
        if self.app_id != other.app_id || self.schema_version != other.schema_version {
            return false;
        }
        let mut a = self.adapters.clone();
        let mut b = other.adapters.clone();
        a.sort_by(|a, b| a.id.cmp(&b.id));
        b.sort_by(|a, b| a.id.cmp(&b.id));
        a == b
    }
    pub fn validate(&self) -> Result<()> {
        bounded(&self.app_id)?;
        bounded(&self.display_name)?;
        if self.adapters.is_empty() || self.adapters.len() > 16 {
            return fail("invalid adapter count");
        }
        let mut ids = std::collections::BTreeSet::new();
        for a in &self.adapters {
            bounded(&a.id)?;
            bounded(&a.namespace)?;
            bounded(&a.schema)?;
            if !ids.insert(&a.id) {
                return fail("duplicate adapter id");
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceMetadata {
    pub display_name: String,
    pub device_kind: String,
    pub role: String,
    pub manifest: AppManifest,
}
impl DeviceMetadata {
    pub fn validate(&self) -> Result<()> {
        bounded(&self.display_name)?;
        bounded(&self.device_kind)?;
        bounded(&self.role)?;
        self.manifest.validate()
    }
}
fn bounded(s: &str) -> Result<()> {
    if s.is_empty() || s.len() > 128 || s.chars().any(char::is_control) {
        fail("invalid metadata field")
    } else {
        Ok(())
    }
}
pub(crate) fn fail<T>(s: &str) -> Result<T> {
    Err(Error::Protocol(format!("pairing: {s}")))
}
fn now() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Protocol("system clock before epoch".into()))?
        .as_secs())
}
fn random_hex(n: usize) -> String {
    let mut bytes = vec![0; n];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Secret-bearing invitation. Treat its encoded value as a credential; never log it.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingInvitation {
    pub version: u32,
    pub invitation_id: String,
    pub inviter_fingerprint: String,
    pub metadata: DeviceMetadata,
    pub expires_at: u64,
    pub secret: String,
}
impl std::fmt::Debug for PairingInvitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingInvitation")
            .field("invitation_id", &self.invitation_id)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}
/// Public connection hints for code entry. Possession confers no trust.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingDescriptor {
    pub version: u32,
    pub invitation_id: String,
    pub inviter_fingerprint: String,
    pub expires_at: u64,
}
impl PairingDescriptor {
    pub fn with_code(&self, metadata: DeviceMetadata, code: &str) -> Result<PairingInvitation> {
        if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return fail("code must contain six digits");
        }
        let invitation = PairingInvitation {
            version: self.version,
            invitation_id: self.invitation_id.clone(),
            inviter_fingerprint: self.inviter_fingerprint.clone(),
            expires_at: self.expires_at,
            metadata,
            secret: code.into(),
        };
        invitation.validate()?;
        Ok(invitation)
    }
}
impl PairingInvitation {
    pub fn descriptor(&self) -> PairingDescriptor {
        PairingDescriptor {
            version: self.version,
            invitation_id: self.invitation_id.clone(),
            inviter_fingerprint: self.inviter_fingerprint.clone(),
            expires_at: self.expires_at,
        }
    }
    pub fn encode(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }
    pub fn decode(encoded: &str) -> Result<Self> {
        if encoded.len() as u64 > MAX_PAIRING_BYTES {
            return fail("invitation too large");
        }
        let invitation: Self = serde_json::from_str(encoded)?;
        invitation.validate()?;
        Ok(invitation)
    }
    pub fn validate(&self) -> Result<()> {
        if self.version != PAIRING_VERSION
            || self.invitation_id.len() != 32
            || !self.invitation_id.bytes().all(|b| b.is_ascii_hexdigit())
            || self.inviter_fingerprint.len() != 64
            || !self
                .inviter_fingerprint
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        {
            return fail("invalid invitation");
        }
        if !((self.secret.len() == 64 && self.secret.bytes().all(|b| b.is_ascii_hexdigit()))
            || (self.secret.len() == 6 && self.secret.bytes().all(|b| b.is_ascii_digit())))
        {
            return fail("invalid invitation credential");
        }
        self.metadata.validate()
    }
}
#[derive(Default)]
pub struct PairingManager {
    invitations: Mutex<BTreeMap<String, InvitationState>>,
}
struct InvitationState {
    invitation: PairingInvitation,
    attempts: u8,
    claimed: bool,
}
impl PairingManager {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn create_invitation(
        &self,
        metadata: DeviceMetadata,
        fingerprint: String,
        ttl: Duration,
    ) -> Result<PairingInvitation> {
        self.create(metadata, fingerprint, ttl, random_hex(32))
    }
    pub fn create_code_invitation(
        &self,
        metadata: DeviceMetadata,
        fingerprint: String,
    ) -> Result<PairingInvitation> {
        self.create_code_invitation_with_ttl(metadata, fingerprint, MAX_TTL)
    }
    pub fn create_code_invitation_with_ttl(
        &self,
        metadata: DeviceMetadata,
        fingerprint: String,
        ttl: Duration,
    ) -> Result<PairingInvitation> {
        // Rejection sampling avoids modulo bias over the million possibilities.
        let number = loop {
            let n = OsRng.next_u32();
            if n < u32::MAX - (u32::MAX % 1_000_000) {
                break n % 1_000_000;
            }
        };
        self.create(metadata, fingerprint, ttl, format!("{number:06}"))
    }
    fn create(
        &self,
        metadata: DeviceMetadata,
        fingerprint: String,
        ttl: Duration,
        secret: String,
    ) -> Result<PairingInvitation> {
        if ttl.is_zero() || ttl > MAX_TTL {
            return fail("invitation lifetime must be at most five minutes");
        }
        let invitation = PairingInvitation {
            version: PAIRING_VERSION,
            invitation_id: random_hex(16),
            inviter_fingerprint: fingerprint,
            metadata,
            expires_at: now()?.saturating_add(ttl.as_secs()),
            secret,
        };
        invitation.validate()?;
        let mut entries = self
            .invitations
            .lock()
            .map_err(|_| Error::Protocol("pairing lock poisoned".into()))?;
        let time = now()?;
        entries.retain(|_, entry| entry.invitation.expires_at > time);
        if entries.len() >= 16 {
            return fail("too many active invitations");
        }
        entries.insert(
            invitation.invitation_id.clone(),
            InvitationState {
                invitation: invitation.clone(),
                attempts: 0,
                claimed: false,
            },
        );
        Ok(invitation)
    }
    pub fn revoke(&self, id: &str) -> Result<()> {
        self.invitations
            .lock()
            .map_err(|_| Error::Protocol("pairing lock poisoned".into()))?
            .remove(id);
        Ok(())
    }
    pub fn close(&self) -> Result<()> {
        self.invitations
            .lock()
            .map_err(|_| Error::Protocol("pairing lock poisoned".into()))?
            .clear();
        Ok(())
    }
    pub fn is_open(&self, id: &str) -> bool {
        self.invitations
            .lock()
            .ok()
            .and_then(|entries| {
                entries.get(id).map(|e| {
                    e.attempts < 5 && !e.claimed && now().is_ok_and(|t| e.invitation.expires_at > t)
                })
            })
            .unwrap_or(false)
    }
    pub(crate) fn claim(&self, id: &str) -> Result<PairingInvitation> {
        let mut entries = self
            .invitations
            .lock()
            .map_err(|_| Error::Protocol("pairing lock poisoned".into()))?;
        let e = entries
            .get_mut(id)
            .ok_or_else(|| Error::Protocol("pairing closed or invitation consumed".into()))?;
        if e.claimed || e.attempts >= 5 || e.invitation.expires_at <= now()? {
            return fail("invitation unavailable");
        }
        e.attempts += 1;
        e.claimed = true;
        Ok(e.invitation.clone())
    }
    pub(crate) fn release(&self, id: &str) {
        if let Ok(mut entries) = self.invitations.lock() {
            if let Some(e) = entries.get_mut(id) {
                e.claimed = false;
            }
        }
    }
    /// Runs the durable approval callback under the invitation lock. Revocation
    /// and expiry are rechecked before committing; revocation cannot race commit.
    pub(crate) fn commit<T>(&self, id: &str, commit: impl FnOnce() -> Result<T>) -> Result<T> {
        let mut entries = self
            .invitations
            .lock()
            .map_err(|_| Error::Protocol("pairing lock poisoned".into()))?;
        let e = entries
            .get(id)
            .ok_or_else(|| Error::Protocol("invitation revoked".into()))?;
        if !e.claimed || e.invitation.expires_at <= now()? {
            return fail("invitation expired");
        }
        let value = commit()?;
        entries.remove(id);
        Ok(value)
    }
}
/// Authenticated enrollment result; metadata must come from the PAKE transcript.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecurePairingOutcome {
    pub invitation_id: String,
    pub identity: Identity,
    pub metadata: DeviceMetadata,
    pub fingerprint: String,
}

use crate::protocol::{read_message_with_limit, write_message};
use crate::{AppKey, DeviceHandler, DeviceKeys, Message, SyncOptions};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Password, Spake2};
use std::io::{BufRead, Write};
use std::net::SocketAddr;

/// Fixed-field structs define the canonical, length-prefixed JSON transcript.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairHello {
    pub version: u32,
    pub invitation_id: String,
    pub identity: Identity,
    pub metadata: DeviceMetadata,
    pub client_fingerprint: String,
    pub server_fingerprint: String,
    pub pake: Vec<u8>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairChallenge {
    pub identity: Identity,
    pub metadata: DeviceMetadata,
    pub pake: Vec<u8>,
    pub confirmation: Vec<u8>,
}
#[derive(Serialize)]
struct Transcript<'a> {
    domain: &'static str,
    hello: &'a PairHello,
    server_identity: &'a Identity,
    server_metadata: &'a DeviceMetadata,
    server_pake: &'a [u8],
}
struct Confirmations {
    client: [u8; 32],
    server: [u8; 32],
    ready: [u8; 32],
    prepared: [u8; 32],
    accepted: [u8; 32],
    commit: [u8; 32],
    transcript: Vec<u8>,
}
impl Confirmations {
    fn new(shared: &[u8], hello: &PairHello, challenge: &PairChallenge) -> Result<Self> {
        let json = serde_json::to_vec(&Transcript {
            domain: "libresync-managed-pair-v2",
            hello,
            server_identity: &challenge.identity,
            server_metadata: &challenge.metadata,
            server_pake: &challenge.pake,
        })?;
        let mut transcript = (json.len() as u32).to_be_bytes().to_vec();
        transcript.extend(json);
        let hash = Sha256::digest(&transcript);
        let hkdf = Hkdf::<Sha256>::new(Some(&hash), shared);
        let mut keys = Self {
            client: [0; 32],
            server: [0; 32],
            ready: [0; 32],
            prepared: [0; 32],
            accepted: [0; 32],
            commit: [0; 32],
            transcript,
        };
        for (label, key) in [
            (b"client-confirm".as_slice(), &mut keys.client),
            (b"server-confirm".as_slice(), &mut keys.server),
            (b"server-ready".as_slice(), &mut keys.ready),
            (b"client-prepared".as_slice(), &mut keys.prepared),
            (b"accepted-key".as_slice(), &mut keys.accepted),
            (b"commit".as_slice(), &mut keys.commit),
        ] {
            hkdf.expand(label, key)
                .map_err(|_| Error::Crypto("pairing HKDF failed".into()))?;
        }
        Ok(keys)
    }
    fn mac(&self, key: &[u8], extra: &[u8]) -> Result<Vec<u8>> {
        let mut mac = Hmac::<Sha256>::new_from_slice(key)
            .map_err(|_| Error::Crypto("pairing HMAC failed".into()))?;
        mac.update(&self.transcript);
        mac.update(extra);
        Ok(mac.finalize().into_bytes().to_vec())
    }
    fn verify(&self, key: &[u8], extra: &[u8], tag: &[u8]) -> Result<()> {
        let mut mac = Hmac::<Sha256>::new_from_slice(key)
            .map_err(|_| Error::Crypto("pairing HMAC failed".into()))?;
        mac.update(&self.transcript);
        mac.update(extra);
        mac.verify_slice(tag)
            .map_err(|_| Error::Protocol("pairing authentication failed".into()))
    }
}
fn pake_ids(id: &str) -> (Vec<u8>, Vec<u8>) {
    (
        format!("libresync-managed-pair-v2/client/{id}").into_bytes(),
        format!("libresync-managed-pair-v2/inviter/{id}").into_bytes(),
    )
}
fn validate_identity(identity: &Identity, metadata: &DeviceMetadata) -> Result<()> {
    bounded(&identity.device_id)?;
    bounded(&identity.app_id)?;
    bounded(&identity.user_id)?;
    metadata.validate()?;
    if identity.app_id != metadata.manifest.app_id {
        return fail("identity and manifest disagree");
    }
    Ok(())
}
fn valid_pake_message(message: &[u8], role: u8) -> bool {
    if message.len() != 33 || message[0] != role {
        return false;
    }
    <Ed25519Group as spake2::Group>::bytes_to_element(&message[1..])
        .is_some_and(|point| point.is_torsion_free() && !point.is_small_order())
}
fn read_pair<R: BufRead>(reader: &mut R) -> Result<Message> {
    read_message_with_limit(reader, MAX_PAIRING_BYTES)
}

/// Pair over mutual TLS and SPAKE2. This blocks; managed SDKs run it on workers.
/// The durable handler callback runs BEFORE the client sends final acceptance.
/// Callers must refuse to replace an established app group's transport key.
pub fn link_secure(
    identity: &Identity,
    metadata: &DeviceMetadata,
    device: SocketAddr,
    device_keys: &DeviceKeys,
    invitation: &PairingInvitation,
    handler: &dyn DeviceHandler,
    options: &SyncOptions,
) -> Result<SecurePairingOutcome> {
    if let Some(cancel) = &options.cancel {
        cancel.check()?;
    }
    let result = link_secure_inner(
        identity,
        metadata,
        device,
        device_keys,
        invitation,
        handler,
        options,
    );
    if let Some(cancel) = &options.cancel {
        cancel.release();
        cancel.check()?;
    }
    result
}
fn link_secure_inner(
    identity: &Identity,
    metadata: &DeviceMetadata,
    device: SocketAddr,
    device_keys: &DeviceKeys,
    invitation: &PairingInvitation,
    handler: &dyn DeviceHandler,
    options: &SyncOptions,
) -> Result<SecurePairingOutcome> {
    validate_identity(identity, metadata)?;
    invitation.validate()?;
    if invitation.expires_at <= now()? {
        return fail("invitation expired");
    }
    if !metadata
        .manifest
        .compatible_with(&invitation.metadata.manifest)
    {
        return fail("incompatible app manifest");
    }
    let pinned = options
        .clone()
        .with_expected_fingerprint(&invitation.inviter_fingerprint);
    let mut reader = crate::sync::open_client(
        device,
        device_keys,
        &pinned,
        &crate::sync::ByteCounters::default(),
    )?;
    let fingerprint = crate::sync::device_fingerprint(reader.get_mut().conn.peer_certificates())?;
    let (a, b) = pake_ids(&invitation.invitation_id);
    let (state, pake) = Spake2::<Ed25519Group>::start_a(
        &Password::new(invitation.secret.as_bytes()),
        &spake2::Identity::new(&a),
        &spake2::Identity::new(&b),
    );
    let hello = PairHello {
        version: PAIRING_VERSION,
        invitation_id: invitation.invitation_id.clone(),
        identity: identity.clone(),
        metadata: metadata.clone(),
        client_fingerprint: device_keys.fingerprint().into(),
        server_fingerprint: fingerprint.clone(),
        pake,
    };
    write_message(reader.get_mut(), &Message::PairHello(hello.clone()))?;
    let challenge = match read_pair(&mut reader)? {
        Message::PairChallenge(c) => c,
        _ => return fail("expected challenge"),
    };
    validate_identity(&challenge.identity, &challenge.metadata)?;
    if !metadata
        .manifest
        .compatible_with(&challenge.metadata.manifest)
        || !invitation
            .metadata
            .manifest
            .compatible_with(&challenge.metadata.manifest)
    {
        return fail("incompatible authenticated manifest");
    }
    if !valid_pake_message(&challenge.pake, b'B') {
        return fail("invalid SPAKE2 group element");
    }
    let shared = state
        .finish(&challenge.pake)
        .map_err(|_| Error::Protocol("invalid SPAKE2 response".into()))?;
    let keys = Confirmations::new(&shared, &hello, &challenge)?;
    keys.verify(&keys.server, &[], &challenge.confirmation)?;
    write_message(
        reader.get_mut(),
        &Message::PairConfirm {
            confirmation: keys.mac(&keys.client, &[])?,
        },
    )?;
    let ready = match read_pair(&mut reader)? {
        Message::PairReady { confirmation } => confirmation,
        _ => return fail("expected authenticated preparation readiness"),
    };
    keys.verify(&keys.ready, &[], &ready)?;
    if let Some(timeout) = authenticated_budget(handler)? {
        reader
            .get_mut()
            .sock
            .inner
            .set_read_timeout(Some(timeout))?;
        reader
            .get_mut()
            .sock
            .inner
            .set_write_timeout(Some(timeout))?;
    }
    if let Some(cancel) = &options.cancel {
        cancel.check()?;
    }
    handler.prepare_secure_pairing(&SecurePairingOutcome {
        invitation_id: invitation.invitation_id.clone(),
        identity: challenge.identity.clone(),
        metadata: challenge.metadata.clone(),
        fingerprint: fingerprint.clone(),
    })?;
    if invitation.expires_at <= now()? {
        return fail("invitation expired before preparation proof");
    }
    if let Some(cancel) = &options.cancel {
        cancel.check()?;
    }
    write_message(
        reader.get_mut(),
        &Message::PairPrepared {
            confirmation: keys.mac(&keys.prepared, &[])?,
        },
    )?;
    let (app_key, confirmation) = match read_pair(&mut reader)? {
        Message::PairAccepted {
            app_key,
            confirmation,
        } => (app_key, confirmation),
        _ => return fail("expected authenticated acceptance"),
    };
    keys.verify(&keys.accepted, &app_key, &confirmation)?;
    let key = AppKey::from_slice(&app_key)?;
    if invitation.expires_at <= now()? {
        return fail("invitation expired before client commit");
    }
    if let Some(cancel) = &options.cancel {
        cancel.check()?;
    }
    handler.commit_secure_pairing(
        &invitation.invitation_id,
        &challenge.identity,
        &challenge.metadata,
        &fingerprint,
        &key,
    )?;
    write_message(
        reader.get_mut(),
        &Message::PairCommit {
            confirmation: keys.mac(&keys.commit, &app_key)?,
        },
    )?;
    if read_pair(&mut reader)? != Message::PairComplete {
        return fail("expected final commit acknowledgement");
    }
    Ok(SecurePairingOutcome {
        invitation_id: invitation.invitation_id.clone(),
        identity: challenge.identity,
        metadata: challenge.metadata,
        fingerprint,
    })
}

pub(crate) fn authenticated_budget(handler: &dyn DeviceHandler) -> Result<Option<Duration>> {
    let timeout = handler.authenticated_pairing_io_timeout();
    if timeout.is_some_and(|t| t.is_zero() || t > Duration::from_secs(300)) {
        return fail("invalid authenticated pairing I/O budget");
    }
    Ok(timeout)
}
pub(crate) fn accept_secure<R: BufRead + Write>(
    reader: &mut std::io::BufReader<R>,
    hello: PairHello,
    identity: &Identity,
    device_keys: &DeviceKeys,
    fingerprint: &str,
    handler: &dyn DeviceHandler,
    authenticated: impl FnOnce(&mut R) -> Result<()>,
) -> Result<()> {
    validate_identity(&hello.identity, &hello.metadata)?;
    if hello.version != PAIRING_VERSION
        || !valid_pake_message(&hello.pake, b'A')
        || hello.client_fingerprint != fingerprint
        || hello.server_fingerprint != device_keys.fingerprint()
    {
        return fail("invalid initial transcript");
    }
    let metadata = handler
        .device_metadata()
        .ok_or_else(|| Error::Protocol("managed pairing not enabled".into()))?;
    validate_identity(identity, &metadata)?;
    if !metadata.manifest.compatible_with(&hello.metadata.manifest) {
        return fail("incompatible manifest");
    }
    let manager = handler
        .pairing_manager()
        .ok_or_else(|| Error::Protocol("pairing closed".into()))?;
    let invitation = manager.claim(&hello.invitation_id)?;
    let result = (|| {
        if invitation.inviter_fingerprint != device_keys.fingerprint()
            || !metadata
                .manifest
                .compatible_with(&invitation.metadata.manifest)
        {
            return fail("invitation identity changed");
        }
        let (a, b) = pake_ids(&invitation.invitation_id);
        let (state, pake) = Spake2::<Ed25519Group>::start_b(
            &Password::new(invitation.secret.as_bytes()),
            &spake2::Identity::new(&a),
            &spake2::Identity::new(&b),
        );
        let shared = state
            .finish(&hello.pake)
            .map_err(|_| Error::Protocol("invalid SPAKE2 request".into()))?;
        let mut challenge = PairChallenge {
            identity: identity.clone(),
            metadata,
            pake,
            confirmation: Vec::new(),
        };
        let keys = Confirmations::new(&shared, &hello, &challenge)?;
        challenge.confirmation = keys.mac(&keys.server, &[])?;
        write_message(reader.get_mut(), &Message::PairChallenge(challenge))?;
        let confirmation = match read_pair(reader)? {
            Message::PairConfirm { confirmation } => confirmation,
            _ => return fail("expected client confirmation"),
        };
        keys.verify(&keys.client, &[], &confirmation)?;
        authenticated(reader.get_mut())?;
        if invitation.expires_at <= now()? {
            return fail("invitation expired before preparation readiness");
        }
        // Send immediately after authentication: no durable app work precedes Ready.
        write_message(
            reader.get_mut(),
            &Message::PairReady {
                confirmation: keys.mac(&keys.ready, &[])?,
            },
        )?;
        let prepared = match read_pair(reader)? {
            Message::PairPrepared { confirmation } => confirmation,
            _ => return fail("expected durable client preparation proof"),
        };
        keys.verify(&keys.prepared, &[], &prepared)?;
        // Inviter key is authoritative; no joining peer-supplied key exists.
        let key = manager.commit(&hello.invitation_id, || {
            let key = handler.app_key()?;
            handler.commit_secure_pairing(
                &hello.invitation_id,
                &hello.identity,
                &hello.metadata,
                fingerprint,
                &key,
            )?;
            Ok(key)
        })?;
        write_message(
            reader.get_mut(),
            &Message::PairAccepted {
                app_key: key.as_bytes().to_vec(),
                confirmation: keys.mac(&keys.accepted, key.as_bytes())?,
            },
        )?;
        let confirmation = match read_pair(reader)? {
            Message::PairCommit { confirmation } => confirmation,
            _ => return fail("expected final client commit"),
        };
        keys.verify(&keys.commit, key.as_bytes(), &confirmation)?;
        // Revocation may have occurred after the durable invitation commit.
        if invitation.expires_at <= now()? {
            return fail("invitation expired before final acceptance");
        }
        if handler
            .recover_secure_pairing(&hello.invitation_id, &hello.identity, fingerprint)?
            .is_none()
        {
            return fail("enrollment revoked before final acceptance");
        }
        write_message(reader.get_mut(), &Message::PairComplete)
    })();
    manager.release(&hello.invitation_id);
    result
}

/// Reconcile an interrupted commit using its journaled identity and TLS pin.
/// This does not reopen an invitation or enroll a new peer.
pub fn recover_secure_link(
    identity: &Identity,
    metadata: &DeviceMetadata,
    device: SocketAddr,
    device_keys: &DeviceKeys,
    previous: &SecurePairingOutcome,
    handler: &dyn DeviceHandler,
    options: &SyncOptions,
) -> Result<SecurePairingOutcome> {
    if let Some(cancel) = &options.cancel {
        cancel.check()?;
    }
    let result = recover_secure_link_inner(
        identity,
        metadata,
        device,
        device_keys,
        previous,
        handler,
        options,
    );
    if let Some(cancel) = &options.cancel {
        cancel.release();
        cancel.check()?;
    }
    result
}
fn recover_secure_link_inner(
    identity: &Identity,
    metadata: &DeviceMetadata,
    device: SocketAddr,
    device_keys: &DeviceKeys,
    previous: &SecurePairingOutcome,
    handler: &dyn DeviceHandler,
    options: &SyncOptions,
) -> Result<SecurePairingOutcome> {
    validate_identity(identity, metadata)?;
    let options = options
        .clone()
        .with_expected_fingerprint(&previous.fingerprint)
        .with_expected_device_id(&previous.identity.device_id);
    let mut reader = crate::sync::open_client(
        device,
        device_keys,
        &options,
        &crate::sync::ByteCounters::default(),
    )?;
    write_message(
        reader.get_mut(),
        &Message::PairRecover {
            invitation_id: previous.invitation_id.clone(),
            identity: identity.clone(),
        },
    )?;
    let (peer, peer_metadata, key) = match read_pair(&mut reader)? {
        Message::PairRecovered {
            identity,
            metadata,
            app_key,
        } => (identity, metadata, app_key),
        _ => return fail("no committed enrollment to recover"),
    };
    if peer != previous.identity || !metadata.manifest.compatible_with(&peer_metadata.manifest) {
        return fail("recovery identity changed");
    }
    if let Some(cancel) = &options.cancel {
        cancel.check()?;
    }
    if let Some(timeout) = authenticated_budget(handler)? {
        reader
            .get_mut()
            .sock
            .inner
            .set_read_timeout(Some(timeout))?;
        reader
            .get_mut()
            .sock
            .inner
            .set_write_timeout(Some(timeout))?;
    }
    handler.commit_secure_pairing(
        &previous.invitation_id,
        &peer,
        &peer_metadata,
        &previous.fingerprint,
        &AppKey::from_slice(&key)?,
    )?;
    write_message(reader.get_mut(), &Message::PairComplete)?;
    Ok(SecurePairingOutcome {
        metadata: peer_metadata,
        ..previous.clone()
    })
}
pub(crate) fn accept_recovery<R: BufRead + Write>(
    reader: &mut std::io::BufReader<R>,
    id: &str,
    peer: &Identity,
    identity: &Identity,
    fingerprint: &str,
    handler: &dyn DeviceHandler,
    authenticated: impl FnOnce(&mut R) -> Result<()>,
) -> Result<()> {
    let peer_metadata = handler
        .recover_secure_pairing(id, peer, fingerprint)?
        .ok_or_else(|| Error::Protocol("no journaled enrollment".into()))?;
    if peer.app_id != identity.app_id {
        return fail("recovery app mismatch");
    }
    let metadata = handler
        .device_metadata()
        .ok_or_else(|| Error::Protocol("missing managed metadata".into()))?;
    if !metadata.manifest.compatible_with(&peer_metadata.manifest) {
        return fail("recovery manifest mismatch");
    }
    authenticated(reader.get_mut())?;
    let key = handler.app_key()?;
    write_message(
        reader.get_mut(),
        &Message::PairRecovered {
            identity: identity.clone(),
            metadata,
            app_key: key.as_bytes().to_vec(),
        },
    )?;
    if read_pair(reader)? != Message::PairComplete {
        return fail("expected recovery acknowledgement");
    }
    Ok(())
}

#[cfg(test)]
mod socket_tests {
    use super::*;
    use crate::{State, SyncListener};
    use std::sync::Arc;
    fn metadata() -> DeviceMetadata {
        DeviceMetadata {
            display_name: "Laptop".into(),
            device_kind: "desktop".into(),
            role: "application".into(),
            manifest: AppManifest {
                app_id: "test.app".into(),
                display_name: "Test".into(),
                schema_version: 1,
                adapters: vec![AdapterDescriptor {
                    id: "ops".into(),
                    namespace: "test.app".into(),
                    schema: "test".into(),
                    transactional: true,
                }],
            },
        }
    }
    struct Handler {
        keys: DeviceKeys,
        manager: Arc<PairingManager>,
        key: AppKey,
        links: Mutex<Vec<SecurePairingOutcome>>,
        prepared: Mutex<Option<SecurePairingOutcome>>,
        delay_ms: std::sync::atomic::AtomicU64,
        budget: Mutex<Option<Duration>>,
    }
    impl DeviceHandler for Handler {
        fn authenticated_pairing_io_timeout(&self) -> Option<Duration> {
            *self.budget.lock().unwrap()
        }
        fn app_id(&self) -> &str {
            "test.app"
        }
        fn is_linked(&self, i: &Identity) -> bool {
            self.links.lock().unwrap().iter().any(|l| l.identity == *i)
        }
        fn approve_link(&self, _: &Identity) -> Result<bool> {
            Ok(false)
        }
        fn app_key(&self) -> Result<AppKey> {
            Ok(self.key.clone())
        }
        fn device_keys(&self) -> Result<DeviceKeys> {
            Ok(self.keys.clone())
        }
        fn device_metadata(&self) -> Option<DeviceMetadata> {
            Some(metadata())
        }
        fn pairing_manager(&self) -> Option<Arc<PairingManager>> {
            Some(self.manager.clone())
        }
        fn allow_legacy_link(&self) -> bool {
            false
        }
        fn prepare_secure_pairing(&self, p: &SecurePairingOutcome) -> Result<()> {
            *self.prepared.lock().unwrap() = Some(p.clone());
            Ok(())
        }
        fn commit_secure_pairing(
            &self,
            id: &str,
            i: &Identity,
            m: &DeviceMetadata,
            f: &str,
            _: &AppKey,
        ) -> Result<()> {
            std::thread::sleep(Duration::from_millis(
                self.delay_ms.load(std::sync::atomic::Ordering::SeqCst),
            ));
            self.links.lock().unwrap().push(SecurePairingOutcome {
                invitation_id: id.into(),
                identity: i.clone(),
                metadata: m.clone(),
                fingerprint: f.into(),
            });
            Ok(())
        }
        fn recover_secure_pairing(
            &self,
            id: &str,
            i: &Identity,
            f: &str,
        ) -> Result<Option<DeviceMetadata>> {
            Ok(self
                .links
                .lock()
                .unwrap()
                .iter()
                .find(|p| p.invitation_id == id && p.identity == *i && p.fingerprint == f)
                .map(|p| p.metadata.clone()))
        }
    }
    fn handler(id: &str) -> Arc<Handler> {
        Arc::new(Handler {
            keys: DeviceKeys::generate(&Identity::new(id, "test.app", "u")).unwrap(),
            manager: Arc::new(PairingManager::new()),
            key: AppKey::generate().unwrap(),
            links: Mutex::new(Vec::new()),
            prepared: Mutex::new(None),
            delay_ms: std::sync::atomic::AtomicU64::new(0),
            budget: Mutex::new(None),
        })
    }
    #[test]
    fn recovery_budget_waits_for_authenticated_slow_durable_commit() {
        for extend in [false, true] {
            let server = handler("server");
            let client = handler("client");
            let client_identity = Identity::new("client", "test.app", "u");
            let server_identity = Identity::new("server", "test.app", "u");
            let previous = SecurePairingOutcome {
                invitation_id: "recovery-budget".into(),
                identity: server_identity.clone(),
                metadata: metadata(),
                fingerprint: server.keys.fingerprint().into(),
            };
            server.links.lock().unwrap().push(SecurePairingOutcome {
                identity: client_identity.clone(),
                fingerprint: client.keys.fingerprint().into(),
                ..previous.clone()
            });
            *client.budget.lock().unwrap() = Some(Duration::from_secs(2));
            client
                .delay_ms
                .store(300, std::sync::atomic::Ordering::SeqCst);
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let worker = std::thread::spawn(move || {
                let (socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_millis(150)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_millis(150)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(
                    crate::sync::tls_server_stream(socket, &server.keys).unwrap(),
                );
                let (id, peer) = match read_pair(&mut reader).unwrap() {
                    Message::PairRecover {
                        invitation_id,
                        identity,
                    } => (invitation_id, identity),
                    _ => panic!("recovery"),
                };
                let fp = crate::sync::device_fingerprint(reader.get_mut().conn.peer_certificates())
                    .unwrap();
                accept_recovery(
                    &mut reader,
                    &id,
                    &peer,
                    &server_identity,
                    &fp,
                    server.as_ref(),
                    |stream| {
                        if extend {
                            stream
                                .sock
                                .inner
                                .set_read_timeout(Some(Duration::from_secs(2)))?;
                            stream
                                .sock
                                .inner
                                .set_write_timeout(Some(Duration::from_secs(2)))?;
                        }
                        Ok(())
                    },
                )
            });
            let result = recover_secure_link(
                &client_identity,
                &metadata(),
                address,
                &client.keys,
                &previous,
                client.as_ref(),
                &SyncOptions::default()
                    .with_timeouts(Duration::from_millis(300), Duration::from_millis(150)),
            );
            let server_result = worker.join().unwrap();
            if extend {
                result.unwrap();
                server_result.unwrap();
            } else {
                assert!(
                    server_result.is_err(),
                    "baseline short timeout unexpectedly covered slow commit"
                );
            }
        }
    }
    #[test]
    fn untrusted_or_revoked_recovery_never_extends_budget_or_releases_key() {
        let server = handler("server");
        let client = handler("client");
        let peer = Identity::new("client", "test.app", "u");
        server.links.lock().unwrap().push(SecurePairingOutcome {
            invitation_id: "exact-journal".into(),
            identity: peer.clone(),
            metadata: metadata(),
            fingerprint: client.keys.fingerprint().into(),
        });
        for revoked in [false, true] {
            if revoked {
                server.links.lock().unwrap().clear();
            }
            let called = std::sync::atomic::AtomicBool::new(false);
            let mut reader = std::io::BufReader::new(std::io::Cursor::new(Vec::new()));
            let fingerprint = if revoked {
                client.keys.fingerprint()
            } else {
                "wrong-certificate"
            };
            assert!(accept_recovery(
                &mut reader,
                "exact-journal",
                &peer,
                &Identity::new("server", "test.app", "u"),
                fingerprint,
                server.as_ref(),
                |_| {
                    called.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                }
            )
            .is_err());
            assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
            assert!(
                reader.into_inner().into_inner().is_empty(),
                "untrusted recovery disclosed a key"
            );
        }
    }
    fn setup(ttl: Duration) -> (Arc<Handler>, Arc<Handler>, SyncListener, PairingInvitation) {
        let server = handler("server");
        let client = handler("client");
        let l = SyncListener::start_with_options(
            "127.0.0.1:0".parse().unwrap(),
            Identity::new("server", "test.app", "u"),
            Arc::new(Mutex::new(State::new("server"))),
            server.clone(),
            crate::ListenerOptions::default().with_io_timeout(Duration::from_secs(2)),
        )
        .unwrap();
        let invite = server
            .manager
            .create_invitation(metadata(), server.keys.fingerprint().into(), ttl)
            .unwrap();
        (server, client, l, invite)
    }
    fn raw(l: &SyncListener, c: &Handler) -> std::io::BufReader<crate::sync::ClientStream> {
        let stream = std::net::TcpStream::connect(l.addr()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(150)))
            .unwrap();
        std::io::BufReader::new(crate::sync::tls_client_stream(stream, &c.keys).unwrap())
    }
    fn begin(
        l: &SyncListener,
        c: &Handler,
        invite: &PairingInvitation,
    ) -> (
        std::io::BufReader<crate::sync::ClientStream>,
        PairHello,
        PairChallenge,
        Confirmations,
    ) {
        let (a, b) = pake_ids(&invite.invitation_id);
        let (state, pake) = Spake2::<Ed25519Group>::start_a(
            &Password::new(invite.secret.as_bytes()),
            &spake2::Identity::new(&a),
            &spake2::Identity::new(&b),
        );
        let hello = PairHello {
            version: PAIRING_VERSION,
            invitation_id: invite.invitation_id.clone(),
            identity: Identity::new("client", "test.app", "u"),
            metadata: metadata(),
            client_fingerprint: c.keys.fingerprint().into(),
            server_fingerprint: invite.inviter_fingerprint.clone(),
            pake,
        };
        let mut r = raw(l, c);
        // Allow the legitimate PAKE challenge to complete on busy test hosts.
        r.get_mut()
            .sock
            .inner
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write_message(r.get_mut(), &Message::PairHello(hello.clone())).unwrap();
        let challenge = match read_pair(&mut r).unwrap() {
            Message::PairChallenge(c) => c,
            _ => panic!("challenge"),
        };
        let shared = state.finish(&challenge.pake).unwrap();
        let keys = Confirmations::new(&shared, &hello, &challenge).unwrap();
        keys.verify(&keys.server, &[], &challenge.confirmation)
            .unwrap();
        // Keep short deadlines for the callers' negative-response assertions.
        r.get_mut()
            .sock
            .inner
            .set_read_timeout(Some(Duration::from_millis(150)))
            .unwrap();
        (r, hello, challenge, keys)
    }
    fn prepared_phase(
        reader: &mut std::io::BufReader<crate::sync::ClientStream>,
        keys: &Confirmations,
    ) {
        let tag = match read_pair(reader).unwrap() {
            Message::PairReady { confirmation } => confirmation,
            _ => panic!("ready"),
        };
        keys.verify(&keys.ready, &[], &tag).unwrap();
        write_message(
            reader.get_mut(),
            &Message::PairPrepared {
                confirmation: keys.mac(&keys.prepared, &[]).unwrap(),
            },
        )
        .unwrap();
    }
    #[test]
    fn wrong_reflected_and_replayed_prepared_proofs_never_enroll_or_disclose_key() {
        let (old_server, old_client, old_listener, old_invitation) =
            setup(Duration::from_secs(300));
        let (old_reader, _, _, old_keys) = begin(&old_listener, &old_client, &old_invitation);
        let replay = old_keys.mac(&old_keys.prepared, &[]).unwrap();
        drop(old_reader);
        old_listener.shutdown().unwrap();
        assert!(old_server.links.lock().unwrap().is_empty());
        for mode in 0..3 {
            let (server, client, listener, invitation) = setup(Duration::from_secs(300));
            let (mut reader, _, _, keys) = begin(&listener, &client, &invitation);
            write_message(
                reader.get_mut(),
                &Message::PairConfirm {
                    confirmation: keys.mac(&keys.client, &[]).unwrap(),
                },
            )
            .unwrap();
            let ready = match read_pair(&mut reader).unwrap() {
                Message::PairReady { confirmation } => confirmation,
                _ => panic!("ready"),
            };
            keys.verify(&keys.ready, &[], &ready).unwrap();
            let proof = match mode {
                0 => vec![0; 32],
                1 => ready,
                _ => replay.clone(),
            };
            write_message(
                reader.get_mut(),
                &Message::PairPrepared {
                    confirmation: proof,
                },
            )
            .unwrap();
            assert!(read_pair(&mut reader).is_err());
            assert!(server.links.lock().unwrap().is_empty());
            listener.shutdown().unwrap();
        }
    }
    #[test]
    fn no_key_before_confirmation_and_disconnect_reserves_one_attempt() {
        let (server, client, l, invite) = setup(Duration::from_secs(300));
        for _ in 0..5 {
            let (mut r, _, _, _) = begin(&l, &client, &invite);
            assert!(read_pair(&mut r).is_err());
            drop(r);
            for _ in 0..20 {
                if server.manager.is_open(&invite.invitation_id) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        assert!(!server.manager.is_open(&invite.invitation_id));
        assert!(server.links.lock().unwrap().is_empty());
        l.shutdown().unwrap();
    }
    #[test]
    fn client_proof_without_durable_prepared_phase_cannot_enroll_or_disclose_key() {
        let (server, client, listener, invitation) = setup(Duration::from_secs(300));
        let (mut reader, _, _, keys) = begin(&listener, &client, &invitation);
        write_message(
            reader.get_mut(),
            &Message::PairConfirm {
                confirmation: keys.mac(&keys.client, &[]).unwrap(),
            },
        )
        .unwrap();
        let response = read_pair(&mut reader).unwrap();
        assert_eq!(
            serde_json::to_value(&response).unwrap()["type"],
            "PairReady",
            "client proof allowed enrollment before durable preparation"
        );
        assert!(server.links.lock().unwrap().is_empty());
        drop(reader);
        listener.shutdown().unwrap();
        assert!(server.links.lock().unwrap().is_empty());
    }
    #[test]
    fn invalid_confirmation_transcript_never_commits() {
        let (server, client, l, invite) = setup(Duration::from_secs(300));
        let (mut r, mut hello, challenge, keys) = begin(&l, &client, &invite);
        hello.metadata.display_name = "Tampered".into();
        let wrong = Confirmations::new(&[0; 32], &hello, &challenge).unwrap();
        let tag = wrong.mac(&wrong.client, &[]).unwrap();
        assert_ne!(tag, keys.mac(&keys.client, &[]).unwrap());
        write_message(r.get_mut(), &Message::PairConfirm { confirmation: tag }).unwrap();
        assert!(read_pair(&mut r).is_err());
        assert!(server.links.lock().unwrap().is_empty());
        l.shutdown().unwrap();
    }
    #[test]
    fn server_expiry_and_revocation_rechecked_at_prepared_commit() {
        for expired in [false, true] {
            let (server, client, l, invite) = setup(Duration::from_secs(300));
            let (mut r, _, _, keys) = begin(&l, &client, &invite);
            assert!(server.links.lock().unwrap().is_empty());
            r.get_mut()
                .sock
                .inner
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            write_message(
                r.get_mut(),
                &Message::PairConfirm {
                    confirmation: keys.mac(&keys.client, &[]).unwrap(),
                },
            )
            .unwrap();
            let confirmation = match read_pair(&mut r).unwrap() {
                Message::PairReady { confirmation } => confirmation,
                _ => panic!("ready"),
            };
            keys.verify(&keys.ready, &[], &confirmation).unwrap();
            assert!(server.links.lock().unwrap().is_empty());
            // Ready has authenticated the still-valid invitation. Change only the
            // authoritative entry before Prepared to exercise the commit recheck.
            if expired {
                let mut invitations = server.manager.invitations.lock().unwrap();
                let state = invitations.get_mut(&invite.invitation_id).unwrap();
                assert!(state.claimed);
                state.invitation.expires_at = 0;
            } else {
                server.manager.revoke(&invite.invitation_id).unwrap();
            }
            r.get_mut()
                .sock
                .inner
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            write_message(
                r.get_mut(),
                &Message::PairPrepared {
                    confirmation: keys.mac(&keys.prepared, &[]).unwrap(),
                },
            )
            .unwrap();
            // Any response, including PairAccepted with the app key, is a failure.
            assert!(matches!(
                read_pair(&mut r),
                Err(Error::Io(error)) if matches!(
                    error.kind(),
                    std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
                )
            ));
            assert!(server.links.lock().unwrap().is_empty());
            l.shutdown().unwrap();
            assert!(server.links.lock().unwrap().is_empty());
        }
    }
    #[test]
    fn lost_acceptance_recovers_from_authenticated_challenge_journal() {
        let (server, client, l, invite) = setup(Duration::from_secs(300));
        let (mut r, _, challenge, keys) = begin(&l, &client, &invite);
        let pending = SecurePairingOutcome {
            invitation_id: invite.invitation_id.clone(),
            identity: challenge.identity,
            metadata: challenge.metadata,
            fingerprint: invite.inviter_fingerprint.clone(),
        };
        client.prepare_secure_pairing(&pending).unwrap();
        write_message(
            r.get_mut(),
            &Message::PairConfirm {
                confirmation: keys.mac(&keys.client, &[]).unwrap(),
            },
        )
        .unwrap();
        prepared_phase(&mut r, &keys);
        drop(r);
        for _ in 0..100 {
            if !server.links.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!server.manager.is_open(&invite.invitation_id));
        let recovered = recover_secure_link(
            &Identity::new("client", "test.app", "u"),
            &metadata(),
            l.addr(),
            &client.keys,
            &client.prepared.lock().unwrap().clone().unwrap(),
            client.as_ref(),
            &SyncOptions::default(),
        )
        .unwrap();
        assert_eq!(pending, recovered);
        assert!(recover_secure_link(
            &Identity::new("client", "test.app", "u"),
            &metadata(),
            l.addr(),
            &handler("client").keys,
            &pending,
            client.as_ref(),
            &SyncOptions::default()
        )
        .is_err());
        l.shutdown().unwrap();
    }
    #[test]
    fn malformed_initial_points_and_transcript_do_not_consume_invitation() {
        let (server, client, l, invite) = setup(Duration::from_secs(300));
        for _ in 0..6 {
            let mut r = raw(&l, &client);
            write_message(
                r.get_mut(),
                &Message::PairHello(PairHello {
                    version: PAIRING_VERSION,
                    invitation_id: invite.invitation_id.clone(),
                    identity: Identity::new("client", "test.app", "u"),
                    metadata: metadata(),
                    client_fingerprint: client.keys.fingerprint().into(),
                    server_fingerprint: invite.inviter_fingerprint.clone(),
                    pake: vec![0; 33],
                }),
            )
            .unwrap();
            assert!(read_pair(&mut r).is_err());
        }
        assert!(server.manager.is_open(&invite.invitation_id));
        assert!(link_secure(
            &Identity::new("client", "test.app", "u"),
            &metadata(),
            l.addr(),
            &client.keys,
            &invite,
            client.as_ref(),
            &SyncOptions::default()
        )
        .is_ok());
        l.shutdown().unwrap();
    }
    #[test]
    fn concurrent_socket_claims_allow_exactly_one_durable_enrollment() {
        let (server, client, l, invite) = setup(Duration::from_secs(300));
        let (mut first, _, _, keys) = begin(&l, &client, &invite);
        let other = handler("other-client");
        assert!(link_secure(
            &Identity::new("other-client", "test.app", "u"),
            &metadata(),
            l.addr(),
            &other.keys,
            &invite,
            other.as_ref(),
            &SyncOptions::default()
        )
        .is_err());
        write_message(
            first.get_mut(),
            &Message::PairConfirm {
                confirmation: keys.mac(&keys.client, &[]).unwrap(),
            },
        )
        .unwrap();
        prepared_phase(&mut first, &keys);
        let key = match read_pair(&mut first).unwrap() {
            Message::PairAccepted {
                app_key,
                confirmation,
            } => {
                keys.verify(&keys.accepted, &app_key, &confirmation)
                    .unwrap();
                app_key
            }
            _ => panic!("accepted"),
        };
        write_message(
            first.get_mut(),
            &Message::PairCommit {
                confirmation: keys.mac(&keys.commit, &key).unwrap(),
            },
        )
        .unwrap();
        assert_eq!(read_pair(&mut first).unwrap(), Message::PairComplete);
        assert_eq!(server.links.lock().unwrap().len(), 1);
        l.shutdown().unwrap();
    }
    #[test]
    fn observed_client_certificate_cannot_be_forged_in_transcript() {
        let (server, client, l, invite) = setup(Duration::from_secs(300));
        let (a, b) = pake_ids(&invite.invitation_id);
        let (_, pake) = Spake2::<Ed25519Group>::start_a(
            &Password::new(invite.secret.as_bytes()),
            &spake2::Identity::new(&a),
            &spake2::Identity::new(&b),
        );
        let mut r = raw(&l, &client);
        write_message(
            r.get_mut(),
            &Message::PairHello(PairHello {
                version: PAIRING_VERSION,
                invitation_id: invite.invitation_id.clone(),
                identity: Identity::new("client", "test.app", "u"),
                metadata: metadata(),
                client_fingerprint: "00".repeat(32),
                server_fingerprint: invite.inviter_fingerprint.clone(),
                pake,
            }),
        )
        .unwrap();
        assert!(read_pair(&mut r).is_err());
        assert!(server.manager.is_open(&invite.invitation_id));
        assert!(server.links.lock().unwrap().is_empty());
        l.shutdown().unwrap();
    }
    #[test]
    fn identity_group_element_is_rejected_before_invitation_claim() {
        let (server, client, l, invite) = setup(Duration::from_secs(300));
        let mut pake = vec![0; 33];
        pake[0] = b'A';
        pake[1] = 1; // Edwards identity encoding.
        let mut r = raw(&l, &client);
        write_message(
            r.get_mut(),
            &Message::PairHello(PairHello {
                version: PAIRING_VERSION,
                invitation_id: invite.invitation_id.clone(),
                identity: Identity::new("client", "test.app", "u"),
                metadata: metadata(),
                client_fingerprint: client.keys.fingerprint().into(),
                server_fingerprint: invite.inviter_fingerprint.clone(),
                pake,
            }),
        )
        .unwrap();
        assert!(read_pair(&mut r).is_err());
        assert!(server.manager.is_open(&invite.invitation_id));
        l.shutdown().unwrap();
    }
    #[test]
    fn point_valid_torsion_and_mixed_torsion_are_rejected_before_claim() {
        let (server, client, l, invite) = setup(Duration::from_secs(300));
        let (a, b) = pake_ids(&invite.invitation_id);
        let (_, honest) = Spake2::<Ed25519Group>::start_a(
            &Password::new(invite.secret.as_bytes()),
            &spake2::Identity::new(&a),
            &spake2::Identity::new(&b),
        );
        let valid = <Ed25519Group as spake2::Group>::bytes_to_element(&honest[1..]).unwrap();
        let torsion = <Ed25519Group as spake2::Group>::bytes_to_element(&[0; 32]).unwrap();
        let mixed = <Ed25519Group as spake2::Group>::add(&valid, &torsion);
        assert!(!mixed.is_torsion_free());
        assert!(!mixed.is_small_order());
        for point in [torsion, mixed] {
            let mut pake = vec![b'A'];
            pake.extend(<Ed25519Group as spake2::Group>::element_to_bytes(&point));
            assert!(!valid_pake_message(&pake, b'A'));
            pake[0] = b'B';
            assert!(!valid_pake_message(&pake, b'B'));
            pake[0] = b'A';
            let mut r = raw(&l, &client);
            write_message(
                r.get_mut(),
                &Message::PairHello(PairHello {
                    version: PAIRING_VERSION,
                    invitation_id: invite.invitation_id.clone(),
                    identity: Identity::new("client", "test.app", "u"),
                    metadata: metadata(),
                    client_fingerprint: client.keys.fingerprint().into(),
                    server_fingerprint: invite.inviter_fingerprint.clone(),
                    pake,
                }),
            )
            .unwrap();
            assert!(read_pair(&mut r).is_err());
            assert!(server.manager.is_open(&invite.invitation_id));
        }
        assert!(link_secure(
            &Identity::new("client", "test.app", "u"),
            &metadata(),
            l.addr(),
            &client.keys,
            &invite,
            client.as_ref(),
            &SyncOptions::default()
        )
        .is_ok());
        l.shutdown().unwrap();
    }
    #[test]
    fn consumed_invitation_rejects_exact_peer_replay() {
        let (_, client, l, invite) = setup(Duration::from_secs(300));
        link_secure(
            &Identity::new("client", "test.app", "u"),
            &metadata(),
            l.addr(),
            &client.keys,
            &invite,
            client.as_ref(),
            &SyncOptions::default(),
        )
        .unwrap();
        assert!(link_secure(
            &Identity::new("client", "test.app", "u"),
            &metadata(),
            l.addr(),
            &client.keys,
            &invite,
            client.as_ref(),
            &SyncOptions::default()
        )
        .is_err());
        l.shutdown().unwrap();
    }
}

#[cfg(test)]
mod transcript_tests {
    use super::*;
    #[test]
    fn every_identity_manifest_certificate_and_pake_field_is_confirmation_bound() {
        let metadata = DeviceMetadata {
            display_name: "Laptop".into(),
            device_kind: "desktop".into(),
            role: "application".into(),
            manifest: AppManifest {
                app_id: "test.app".into(),
                display_name: "Test".into(),
                schema_version: 1,
                adapters: vec![AdapterDescriptor {
                    id: "ops".into(),
                    namespace: "test.app".into(),
                    schema: "v1".into(),
                    transactional: true,
                }],
            },
        };
        let hello = PairHello {
            version: PAIRING_VERSION,
            invitation_id: "00".repeat(16),
            identity: Identity::new("client", "test.app", "u"),
            metadata: metadata.clone(),
            client_fingerprint: "11".repeat(32),
            server_fingerprint: "22".repeat(32),
            pake: vec![65; 33],
        };
        let challenge = PairChallenge {
            identity: Identity::new("server", "test.app", "u"),
            metadata,
            pake: vec![66; 33],
            confirmation: Vec::new(),
        };
        let keys = Confirmations::new(&[7; 32], &hello, &challenge).unwrap();
        let tag = keys.mac(&keys.server, &[]).unwrap();
        assert!(keys.verify(&keys.client, &[], &tag).is_err());
        for field in 0..15 {
            let mut h = hello.clone();
            let mut c = challenge.clone();
            match field {
                0 => h.version += 1,
                1 => h.invitation_id.push('0'),
                2 => h.identity.device_id.push('x'),
                3 => h.identity.user_id.push('x'),
                4 => h.identity.app_id.push('x'),
                5 => h.client_fingerprint.push('x'),
                6 => h.server_fingerprint.push('x'),
                7 => h.metadata.manifest.schema_version += 1,
                8 => h.metadata.manifest.adapters[0].namespace.push('x'),
                9 => h.metadata.display_name.push('x'),
                10 => h.pake[2] ^= 1,
                11 => c.identity.device_id.push('x'),
                12 => c.identity.user_id.push('x'),
                13 => c.metadata.manifest.adapters[0].schema.push('x'),
                14 => c.pake[2] ^= 1,
                _ => unreachable!(),
            };
            let changed = Confirmations::new(&[7; 32], &h, &c).unwrap();
            assert!(
                changed.verify(&changed.server, &[], &tag).is_err(),
                "unbound field {field}"
            );
        }
    }
}
