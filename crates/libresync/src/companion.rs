//! Isolated, schema-declared store-and-forward spaces. No application Applied
//! acknowledgement is fabricated by the companion.
use crate::*;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

pub const MOMENTUM_APP_ID: &str = "io.github.dan_hart.Momentum";
pub const MAX_SNAPSHOT_COMPRESSED: usize = 32 * 1024 * 1024;
pub const MAX_SNAPSHOT_EXPANDED: usize = 128 * 1024 * 1024;
pub fn momentum_manifest() -> AppManifest {
    AppManifest {
        app_id: MOMENTUM_APP_ID.into(),
        display_name: "Momentum".into(),
        schema_version: 1,
        adapters: vec![AdapterDescriptor {
            id: "ops".into(),
            namespace: MOMENTUM_APP_ID.into(),
            schema: "momentum".into(),
            transactional: true,
        }],
    }
}
fn invalid<T>(message: &str) -> Result<T> {
    Err(Error::Managed {
        code: SessionErrorCode::InvalidRecord,
        message: message.into(),
    })
}
/// Policy for the actual sp-p2p wire contract at Momentum commit 766064b.
/// Operations are immutable; later app-issued tombstones remain valid.
/// Snapshots use last-writer-wins. Nothing is compacted based on age.
pub struct MomentumAdapter;
impl MomentumAdapter {
    pub fn encode(record: &SyncRecord) -> Result<ManagedRecord> {
        let value = serde_json::to_vec(record)?;
        let wire = ManagedRecord {
            adapter: "ops".into(),
            id: record_entry_key(MOMENTUM_APP_ID, &record.schema, &record.entity, &record.id),
            value,
            deleted: record.tombstone,
            clock: record.clock.clone(),
        };
        Self::decode(&wire)?;
        Ok(wire)
    }
    pub fn decode(wire: &ManagedRecord) -> Result<SyncRecord> {
        let record: SyncRecord = serde_json::from_slice(&wire.value)?;
        if wire.adapter != "ops"
            || record.schema != "momentum"
            || record.id.is_empty()
            || !matches!(record.entity.as_str(), "Op" | "Snapshot")
            || (record.entity == "Snapshot" && record.id != "latest")
            || wire.id
                != record_entry_key(MOMENTUM_APP_ID, &record.schema, &record.entity, &record.id)
            || wire.deleted != record.tombstone
            || wire.clock != record.clock
        {
            return invalid("Momentum record envelope or schema mismatch");
        }
        if !record.tombstone {
            if !matches!(record.fields.get("t"), Some(FieldValue::I64(_)))
                || !matches!(record.fields.get("origin"), Some(FieldValue::String(s)) if !s.is_empty())
            {
                return invalid("Momentum timestamp or origin absent");
            }
            if record.entity == "Op" {
                for name in ["op", "action"] {
                    match record.fields.get(name) {
                        Some(FieldValue::String(s)) => {
                            let value: serde_json::Value = serde_json::from_str(s)?;
                            if !value.is_object() {
                                return invalid("Momentum operation must be an object");
                            }
                        }
                        _ => return invalid("Momentum operation fields absent"),
                    }
                }
            } else {
                match record.fields.get("gz_b64") {
                    Some(FieldValue::String(s)) => {
                        Self::validate_snapshot(s, MAX_SNAPSHOT_COMPRESSED, MAX_SNAPSHOT_EXPANDED)?;
                    }
                    _ => return invalid("Momentum snapshot absent"),
                }
            }
        }
        Ok(record)
    }
    pub fn validate_snapshot(
        encoded: &str,
        compressed_limit: usize,
        expanded_limit: usize,
    ) -> Result<()> {
        if encoded.len() > compressed_limit.saturating_add(2) / 3 * 4 {
            return invalid("Momentum snapshot exceeds compressed limit");
        }
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| Error::Protocol("invalid snapshot base64".into()))?;
        if bytes.len() > compressed_limit {
            return invalid("Momentum snapshot exceeds compressed limit");
        }
        let mut decoder =
            flate2::read::GzDecoder::new(bytes.as_slice()).take(expanded_limit as u64 + 1);
        let mut expanded = Vec::new();
        decoder.read_to_end(&mut expanded)?;
        if expanded.len() > expanded_limit {
            return invalid("Momentum snapshot exceeds expanded limit");
        }
        let data: serde_json::Value = serde_json::from_slice(&expanded)?;
        if !data.is_object() {
            return invalid("Momentum snapshot must contain AppData object");
        }
        Ok(())
    }
}
impl ManagedAdapter for MomentumAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        momentum_manifest().adapters.remove(0)
    }
    fn prepare(
        &self,
        local: &[ManagedRecord],
        incoming: &[ManagedRecord],
        revision: u64,
    ) -> Result<PreparedChange> {
        let mut merged: BTreeMap<String, ManagedRecord> = BTreeMap::new();
        for record in local {
            Self::decode(record)?;
            merged.insert(record.id.clone(), record.clone());
        }
        for next in incoming {
            let decoded = Self::decode(next)?;
            if let Some(old) = merged.get(&next.id) {
                let previous = Self::decode(old)?;
                if decoded.entity == "Op"
                    && !decoded.tombstone
                    && !previous.tombstone
                    && decoded.fields != previous.fields
                {
                    return invalid("immutable Momentum operation collision");
                }
                if old.clock == next.clock && old != next {
                    return invalid("Momentum collision at equal clock");
                }
                if previous.entity == "Op" && previous.tombstone && !decoded.tombstone {
                    continue;
                }
                if old.clock >= next.clock {
                    continue;
                }
            }
            merged.insert(next.id.clone(), next.clone());
        }
        Ok(PreparedChange {
            id: random_id(),
            adapter: "ops".into(),
            expected_revision: revision,
            staged: merged.into_values().collect(),
            recovery: local.to_vec(),
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppSpace {
    pub id: String,
    pub manifest: AppManifest,
    pub paused: bool,
    /// Retained encrypted exports; no whole-op-log rollback is offered.
    pub retain_backups: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpaceSnapshot {
    pub space: AppSpace,
    pub session: SessionSnapshot,
    pub recovery_copies: usize,
    pub recovery: Vec<RecoverySummary>,
    pub backups: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoverySummary {
    pub id: String,
    pub revision: u64,
    pub records: usize,
}
#[derive(Clone, Serialize, Deserialize, Default)]
struct Registry {
    spaces: BTreeMap<String, AppSpace>,
}
pub struct CompanionManager {
    root: PathBuf,
    device_name: String,
    keys: Arc<dyn KeyStore>,
    advertise: bool,
    registry: Registry,
    sessions: BTreeMap<String, Arc<Session>>,
    catalog: Vec<AppSupport>,
    fenced: bool,
    _lease: crate::lease::StateLease,
}
impl Drop for CompanionManager {
    fn drop(&mut self) {
        // Also stop sessions retained by callers. The final lease field drops only
        // after shutdown and the manager's session references have been dropped.
        let _ = self.shutdown();
    }
}

struct SpaceKeys {
    prefix: String,
    store: Arc<dyn KeyStore>,
}
impl KeyStore for SpaceKeys {
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>> {
        self.store.get(&format!("{}-{name}", self.prefix))
    }
    fn set(&self, name: &str, value: &[u8]) -> Result<()> {
        self.store.set(&format!("{}-{name}", self.prefix), value)
    }
    fn delete(&self, name: &str) -> Result<()> {
        self.store.delete(&format!("{}-{name}", self.prefix))
    }
}
fn random_id() -> String {
    use rand_core::{OsRng, RngCore};
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
fn atomic(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Protocol("parent absent".into()))?;
    private_dir(parent)?;
    let temp = parent.join(format!(".{}.tmp", random_id()));
    let mut renamed = false;
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(data)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        renamed = true;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
        if renamed {
            return Err(Error::Managed {
                code: SessionErrorCode::StorageCommitUncertain,
                message:
                    "Companion storage durability is uncertain. Close and reopen for recovery."
                        .into(),
            });
        }
    }
    result
}
impl CompanionManager {
    pub fn open(
        root: impl Into<PathBuf>,
        device_name: &str,
        keys: Arc<dyn KeyStore>,
        advertise: bool,
    ) -> Result<Self> {
        Self::open_with_catalog(root, device_name, keys, advertise, default_catalog())
    }
    pub fn open_with_catalog(
        root: impl Into<PathBuf>,
        device_name: &str,
        keys: Arc<dyn KeyStore>,
        advertise: bool,
        catalog: Vec<AppSupport>,
    ) -> Result<Self> {
        for support in &catalog {
            support.validate()?;
        }
        let root = root.into();
        private_dir(&root)?;
        let root = fs::canonicalize(root)?;
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lease = crate::lease::StateLease::acquire(
            options.open(root.join("manager.lock"))?,
            "companion directory",
        )?;
        let registry = match fs::read(root.join("spaces.json")) {
            Ok(bytes) => serde_json::from_slice::<Registry>(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Registry::default(),
            Err(e) => return Err(e.into()),
        };
        let mut manager = Self {
            root,
            device_name: device_name.into(),
            keys,
            advertise,
            registry,
            sessions: BTreeMap::new(),
            catalog,
            fenced: false,
            _lease: lease,
        };
        for (id, space) in &manager.registry.spaces {
            if id != &space.id
                || id.len() != 32
                || !id.bytes().all(|b| b.is_ascii_hexdigit())
                || !(1..=20).contains(&space.retain_backups)
            {
                return Err(Error::Protocol("invalid companion registry".into()));
            }
            let session = manager.open_space(space)?;
            if space.paused {
                session.pause()?;
            } else {
                session.start()?;
            }
            manager.sessions.insert(id.clone(), Arc::new(session));
        }
        Ok(manager)
    }
    fn open_space(&self, space: &AppSpace) -> Result<Session> {
        let support = self.catalog.iter().find(|s|s.manifest.compatible_with(&space.manifest)).ok_or_else(||Error::Managed{code:SessionErrorCode::IncompatibleSchema,message:"This app schema is not supported. Upgrade both apps or install a matching adapter.".into()})?;
        let mut config = SessionConfig::new(
            self.root.join("spaces").join(&space.id),
            DeviceMetadata {
                display_name: self.device_name.clone(),
                device_kind: "desktop".into(),
                role: "companion".into(),
                manifest: space.manifest.clone(),
            },
        );
        config.advertise = self.advertise;
        config.recovery_retention = 5;
        for adapter in &support.adapters {
            config = config.with_adapter(adapter.clone())?;
        }
        Session::open(
            config,
            Arc::new(SpaceKeys {
                prefix: space.id.clone(),
                store: self.keys.clone(),
            }),
        )
    }
    pub fn add_app(&mut self, manifest: AppManifest) -> Result<String> {
        manifest.validate()?;
        let space = AppSpace {
            id: random_id(),
            manifest,
            paused: false,
            retain_backups: 5,
        };
        let session = self.open_space(&space)?;
        session.start()?;
        let id = space.id.clone();
        let mut candidate = self.registry.clone();
        candidate.spaces.insert(id.clone(), space);
        self.persist_registry(candidate)?;
        self.sessions.insert(id.clone(), Arc::new(session));
        Ok(id)
    }
    /// Persists an empty supported space first. Only successful authenticated
    /// pairing grants trust; failed enrollment leaves a visible repairable space.
    pub fn enroll(&mut self, invitation: &SessionInvitation) -> Result<String> {
        let id = self.add_app(invitation.invitation.metadata.manifest.clone())?;
        self.session(&id)?.connect(invitation)?;
        Ok(id)
    }
    pub fn session(&self, id: &str) -> Result<Arc<Session>> {
        if self.fenced {
            return Err(Error::Managed {
                code: SessionErrorCode::StorageCommitUncertain,
                message: "Companion fenced; reopen to recover storage".into(),
            });
        }
        self.sessions
            .get(id)
            .cloned()
            .ok_or_else(|| Error::Protocol("app space absent".into()))
    }
    pub fn snapshots(&self) -> Result<Vec<SpaceSnapshot>> {
        self.registry
            .spaces
            .values()
            .map(|space| {
                let session = self.session(&space.id)?;
                let recovery = session.recovery_snapshot_sizes()?;
                Ok(SpaceSnapshot {
                    space: space.clone(),
                    session: session.snapshot()?,
                    recovery_copies: recovery.len(),
                    recovery: recovery
                        .into_iter()
                        .map(|(id, revision, records)| RecoverySummary {
                            id,
                            revision,
                            records,
                        })
                        .collect(),
                    backups: self.backups(&space.id)?,
                })
            })
            .collect()
    }
    fn persist_registry(&mut self, candidate: Registry) -> Result<()> {
        if self.fenced {
            return Err(Error::Managed {
                code: SessionErrorCode::StorageCommitUncertain,
                message: "Companion fenced; reopen to recover storage".into(),
            });
        }
        if let Err(error) = atomic(
            &self.root.join("spaces.json"),
            &serde_json::to_vec(&candidate)?,
        ) {
            if matches!(
                error,
                Error::Managed {
                    code: SessionErrorCode::StorageCommitUncertain,
                    ..
                }
            ) {
                self.fenced = true;
                for session in self.sessions.values() {
                    let _ = session.shutdown();
                }
            }
            return Err(error);
        }
        self.registry = candidate;
        Ok(())
    }
    pub fn supported_apps(&self) -> Vec<AppManifest> {
        self.catalog.iter().map(|s| s.manifest.clone()).collect()
    }
    pub fn pause(&mut self, id: &str) -> Result<()> {
        let session = self.session(id)?;
        let mut candidate = self.registry.clone();
        candidate.spaces.get_mut(id).ok_or(Error::Cancelled)?.paused = true;
        self.persist_registry(candidate)?;
        session.pause()
    }
    pub fn resume(&mut self, id: &str) -> Result<()> {
        let session = self.session(id)?;
        let mut candidate = self.registry.clone();
        candidate.spaces.get_mut(id).ok_or(Error::Cancelled)?.paused = false;
        self.persist_registry(candidate)?;
        session.resume().map(|_| ())
    }
    /// Stop and archive this space. Copies and keys are preserved on disk;
    /// remote copies cannot be erased. Re-adding creates a separate trust group.
    pub fn remove(&mut self, id: &str) -> Result<()> {
        let session = self.session(id)?;
        let mut candidate = self.registry.clone();
        candidate.spaces.remove(id);
        self.persist_registry(candidate)?;
        let revoke: Result<()> = (|| {
            for peer in session.snapshot()?.peers {
                session.remove_peer(&peer.identity.device_id)?;
            }
            Ok(())
        })();
        let stop = session.shutdown();
        self.sessions.remove(id);
        revoke?;
        stop
    }
    pub fn backups(&self, id: &str) -> Result<Vec<String>> {
        self.session(id)?;
        let dir = self.root.join("backups").join(id);
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut names = fs::read_dir(dir)?
            .map(|e| Ok(e?.file_name().to_string_lossy().into_owned()))
            .collect::<Result<Vec<_>>>()?;
        names.retain(|s| s.ends_with(".enc") && !s.starts_with('.'));
        names.sort();
        Ok(names)
    }
    pub fn backup(&self, id: &str) -> Result<String> {
        let session = self.session(id)?;
        let inbox = session.application_inbox()?;
        let key_name = format!("{id}-backup-key-v1");
        let key = match self.keys.get(&key_name)? {
            Some(b) => AppKey::from_slice(&b)?,
            None => {
                if !self.backups(id)?.is_empty() {
                    return Err(Error::Managed{code:SessionErrorCode::StorageUnavailable,message:"Backup key is missing for existing recovery copies. Restore secure storage; no key was replaced.".into()});
                }
                let k = AppKey::generate()?;
                self.keys.set(&key_name, k.as_bytes())?;
                k
            }
        };
        let aad = format!("libresync-companion-backup-v1:{id}");
        let cipher = encrypt_blob(&key, &serde_json::to_vec(&inbox)?, aad.as_bytes())?;
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Error::Protocol("clock before epoch".into()))?
            .as_millis();
        let name = format!("{timestamp:020}-{}.enc", random_id());
        let dir = self.root.join("backups").join(id);
        atomic(&dir.join(&name), &cipher)?;
        let retain = self
            .registry
            .spaces
            .get(id)
            .ok_or(Error::Cancelled)?
            .retain_backups;
        let names = self.backups(id)?;
        for old in names.iter().take(names.len().saturating_sub(retain)) {
            fs::remove_file(dir.join(old))?;
        }
        fs::File::open(&dir)?.sync_all()?;
        session.prune_recovery_snapshots(retain)?;
        Ok(name)
    }
    /// Export decrypted logical recovery data only following explicit user
    /// confirmation in the caller. Does not change data, trust, or receipts.
    pub fn export_backup(&self, id: &str, name: &str) -> Result<ApplicationInbox> {
        if !self.backups(id)?.contains(&name.to_string()) {
            return Err(Error::Protocol("backup absent".into()));
        }
        let bytes = self
            .keys
            .get(&format!("{id}-backup-key-v1"))?
            .ok_or_else(|| Error::Protocol("backup key unavailable".into()))?;
        let key = AppKey::from_slice(&bytes)?;
        let cipher = fs::read(self.root.join("backups").join(id).join(name))?;
        Ok(serde_json::from_slice(&decrypt_blob(
            &key,
            &cipher,
            format!("libresync-companion-backup-v1:{id}").as_bytes(),
        )?)?)
    }
}
/// Select secure OS storage without silent plaintext fallback. Headless file
/// storage is a separately explicit operator choice.
pub fn secure_companion_keys() -> Result<Arc<dyn KeyStore>> {
    #[cfg(target_os = "macos")]
    {
        Ok(Arc::new(NativeKeychainKeyStore::new(
            "LibreSyncAlwaysOn-managed",
        )))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let store = SecretToolKeyStore::new("LibreSyncAlwaysOn-managed");
        if store.is_available() {
            return Ok(Arc::new(store));
        }
    }
    #[cfg(not(target_os = "macos"))]
    Err(Error::Managed {
        code: SessionErrorCode::StorageUnavailable,
        message:
            "Secure OS key storage is unavailable. Unlock your keychain or enable Secret Service."
                .into(),
    })
}
/// Offline QR SVG. Only fixed geometry is rendered; payload is never SVG text.
pub fn invitation_qr(invitation: &SessionInvitation) -> Result<String> {
    let code = qrcode::QrCode::new(invitation.encode()?.as_bytes())
        .map_err(|_| Error::Protocol("invitation too large for QR".into()))?;
    let width = code.width();
    let mut svg=format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {} {}\"><rect width=\"100%\" height=\"100%\" fill=\"white\"/><path fill=\"black\" d=\"",width+8,width+8);
    for y in 0..width {
        for x in 0..width {
            if code[(x, y)] == qrcode::Color::Dark {
                svg.push_str(&format!("M{} {}h1v1h-1z", x + 4, y + 4));
            }
        }
    }
    svg.push_str("\"/></svg>");
    Ok(svg)
}
impl CompanionManager {
    /// Explicit migration recovery copy. Legacy Engine trust and keys are never
    /// activated in managed sessions. Original paths are left untouched.
    pub fn archive_legacy(&mut self, config_path: &Path) -> Result<PathBuf> {
        let config_bytes = fs::read(config_path)?;
        let config: serde_json::Value = serde_json::from_slice(&config_bytes)?;
        let parent = self.root.join("legacy-archives");
        private_dir(&parent)?;
        let pending = parent.join(format!(".pending-{}", random_id()));
        private_dir(&pending)?;
        let copied: Result<()> = (|| {
            atomic(&pending.join("config.json"), &config_bytes)?;
            for (field, name) in [
                ("data_path", "libresync.json"),
                ("state_path", "state.json"),
            ] {
                if let Some(path) = config.get(field).and_then(|v| v.as_str()) {
                    let original = Path::new(path);
                    let source = if original.is_absolute() {
                        original.to_path_buf()
                    } else {
                        config_path
                            .parent()
                            .unwrap_or(Path::new("."))
                            .join(original)
                    };
                    if source.exists() {
                        atomic(&pending.join(name), &fs::read(source)?)?;
                    }
                }
            }
            atomic(
                &pending.join("migration.json"),
                &serde_json::to_vec(
                    &serde_json::json!({"version":1,"original_config":config_path,"mode":"legacy-recovery-only","network_enabled":false,"automatic_approval":false,"retention_copies":5,"message":"Legacy keys, state and data are recovery copies. No legacy trust is imported. Connect a compatible managed app to a new space after reviewing its app migration."}),
                )?,
            )?;
            Ok(())
        })();
        if let Err(error) = copied {
            fs::remove_dir_all(&pending)?;
            return Err(error);
        }
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Error::Protocol("clock before epoch".into()))?
            .as_nanos();
        let archive = parent.join(format!("{timestamp:025}-{}", random_id()));
        fs::rename(&pending, &archive)?;
        fs::File::open(&parent)?.sync_all()?;
        // Only completed, generated migration directories are eligible. Never
        // prune original paths, unrelated directories, or live managed state.
        let mut completed = Vec::new();
        for entry in fs::read_dir(&parent)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let generated = name.len() == 58
                && name.as_bytes()[25] == b'-'
                && name.as_bytes()[..25].iter().all(u8::is_ascii_digit)
                && name.as_bytes()[26..].iter().all(u8::is_ascii_hexdigit);
            if generated && entry.file_type()?.is_dir() {
                match fs::metadata(entry.path().join("migration.json")) {
                    Ok(metadata) if metadata.is_file() => completed.push(entry.path()),
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        completed.sort();
        for old in completed.iter().take(completed.len().saturating_sub(5)) {
            fs::remove_dir_all(old)?;
        }
        fs::File::open(&parent)?.sync_all()?;
        Ok(archive)
    }
}
/// Explicit generic logical records contract, distinct from JsonFileAdapter.
pub fn notes_manifest() -> AppManifest {
    AppManifest {
        app_id: "io.libresync.Notes".into(),
        display_name: "Local notes sample".into(),
        schema_version: 1,
        adapters: vec![AdapterDescriptor {
            id: "records".into(),
            namespace: "io.libresync.Notes".into(),
            schema: "logical-records-v1".into(),
            transactional: true,
        }],
    }
}

/// An operator/application registered schema policy. An invitation cannot add
/// entries to this trusted catalog. Descriptors must match pure adapters.
#[derive(Clone)]
pub struct AppSupport {
    pub manifest: AppManifest,
    pub adapters: Vec<Arc<dyn ManagedAdapter>>,
}
impl AppSupport {
    fn validate(&self) -> Result<()> {
        self.manifest.validate()?;
        let mut descriptors = self
            .adapters
            .iter()
            .map(|a| a.descriptor())
            .collect::<Vec<_>>();
        descriptors.sort_by(|a, b| a.id.cmp(&b.id));
        let mut declared = self.manifest.adapters.clone();
        declared.sort_by(|a, b| a.id.cmp(&b.id));
        if descriptors != declared || descriptors.iter().any(|a| !a.transactional) {
            return Err(Error::Managed {
                code: SessionErrorCode::InvalidAdapter,
                message: "Catalog policies differ from declared schema adapters".into(),
            });
        }
        Ok(())
    }
}
pub fn default_catalog() -> Vec<AppSupport> {
    let notes = notes_manifest();
    vec![
        AppSupport {
            manifest: momentum_manifest(),
            adapters: vec![Arc::new(MomentumAdapter)],
        },
        AppSupport {
            adapters: vec![Arc::new(RecordsAdapter::new(notes.adapters[0].clone()))],
            manifest: notes,
        },
    ]
}

/// Inspect ONLY the previous LibreSync tray config in the historical Tauri
/// default data path. Other apps can share that parent, so do not migrate it.
pub fn legacy_config_available(path: &Path) -> bool {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|config| {
            config.get("app_id").and_then(|v| v.as_str()) == Some("com.codedbydan.libresync")
                && config.get("state_path").is_some_and(|v| v.is_string())
                && config.get("data_path").is_some_and(|v| v.is_string())
        })
}
impl CompanionManager {
    pub fn shutdown(&self) -> Result<()> {
        let mut first = None;
        for session in self.sessions.values() {
            if let Err(error) = session.shutdown() {
                if first.is_none() {
                    first = Some(error);
                }
            }
        }
        match first {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum DiscoveryCompatibility {
    Ready,
    DifferentApp,
    UpgradeSchema,
    UpgradePairing,
    MissingMetadata,
    PairingClosed,
    Expired,
}
/// Discovery is an untrusted hint. Authenticated pairing always revalidates the
/// full manifest and identity. No descriptor implies no open managed pairing.
pub fn discovery_compatibility(
    manifest: &AppManifest,
    peer: &DiscoveredPeer,
    now: u64,
) -> Result<DiscoveryCompatibility> {
    if peer.identity.app_id != manifest.app_id {
        return Ok(DiscoveryCompatibility::DifferentApp);
    }
    let Some(advertisement) = &peer.advertisement else {
        return Ok(DiscoveryCompatibility::MissingMetadata);
    };
    if !advertisement.compatible_with(manifest)? {
        return Ok(DiscoveryCompatibility::UpgradeSchema);
    }
    let Some(invitation) = &peer.invitation else {
        return Ok(DiscoveryCompatibility::PairingClosed);
    };
    if invitation.version != PAIRING_VERSION {
        return Ok(DiscoveryCompatibility::UpgradePairing);
    }
    if invitation.expires_at <= now {
        return Ok(DiscoveryCompatibility::Expired);
    }
    Ok(DiscoveryCompatibility::Ready)
}
impl CompanionManager {
    pub fn nearby_devices(&self, id: &str) -> Result<serde_json::Value> {
        let manifest = &self
            .registry
            .spaces
            .get(id)
            .ok_or(Error::Cancelled)?
            .manifest;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Error::Protocol("clock before epoch".into()))?
            .as_secs();
        let peers = self.session(id)?.discovered_peers()?;
        let views = peers
            .into_iter()
            .map(|peer| {
                let compatibility = discovery_compatibility(manifest, &peer, now)?;
                Ok(serde_json::json!({"peer":peer,"compatibility":compatibility}))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(serde_json::to_value(views)?)
    }
}

impl CompanionManager {
    /// Export a pre-merge record copy. This never restores state or rewinds an
    /// operation log, trust, cursors, or application receipts.
    pub fn export_recovery(&self, id: &str, snapshot: &str) -> Result<RecoverySnapshot> {
        self.session(id)?
            .recovery_snapshots()?
            .into_iter()
            .find(|r| r.id == snapshot)
            .ok_or_else(|| Error::Protocol("pre-merge recovery copy absent".into()))
    }
}

#[cfg(test)]
mod lease_tests {
    use super::*;

    #[test]
    fn companion_drop_stops_sessions_retained_by_callers() {
        let root = tempfile::tempdir().unwrap();
        let keys = Arc::new(MemoryKeyStore::new());
        let mut manager = CompanionManager::open(root.path(), "Lease test", keys, false).unwrap();
        let id = manager.add_app(notes_manifest()).unwrap();
        let retained = manager.session(&id).unwrap();
        assert_eq!(retained.snapshot().unwrap().phase, SessionPhase::Running);
        drop(manager);
        assert_eq!(retained.snapshot().unwrap().phase, SessionPhase::Stopped);
    }

    #[test]
    fn companion_owner_drop_releases_lease_with_retained_descriptor() {
        let root = tempfile::tempdir().unwrap();
        let keys = Arc::new(MemoryKeyStore::new());
        let mut manager =
            CompanionManager::open(root.path(), "Lease test", keys.clone(), false).unwrap();
        manager.add_app(notes_manifest()).unwrap();
        let inherited = manager._lease.try_clone().unwrap();
        assert!(CompanionManager::open(root.path(), "Lease test", keys.clone(), false).is_err());
        drop(manager);
        let reopened =
            CompanionManager::open(root.path(), "Lease test", keys.clone(), false).unwrap();
        assert!(CompanionManager::open(root.path(), "Lease test", keys.clone(), false).is_err());
        drop(reopened);
        // A persisted space unsupported by the caller catalog must release its lease.
        assert!(CompanionManager::open_with_catalog(
            root.path(),
            "Lease test",
            keys.clone(),
            false,
            vec![]
        )
        .is_err());
        let recovered = CompanionManager::open(root.path(), "Lease test", keys, false).unwrap();
        drop(recovered);
        drop(inherited);
    }
}
