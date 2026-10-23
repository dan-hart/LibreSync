use super::{fail, random_id, Envelope, SessionConfig};
use crate::{decrypt_blob, encrypt_blob, AppKey, DeviceKeys, Identity, KeyStore, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
};
#[derive(Serialize, Deserialize)]
struct IdentityBundle {
    identity: Identity,
    certificate: Vec<u8>,
    private_key: Vec<u8>,
    storage_key: Vec<u8>,
}
fn aad(config: &SessionConfig) -> Vec<u8> {
    format!(
        "libresync-managed-store-v1:{}",
        config.metadata.manifest.app_id
    )
    .into_bytes()
}
const REFERENCE: &str = "identity-ref.json";
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityReference {
    version: u32,
    account: String,
}
fn valid_account(name: &str) -> bool {
    name.strip_prefix("managed-")
        .and_then(|s| s.strip_suffix("-identity-v1"))
        .is_some_and(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}
fn legacy_account(config: &SessionConfig) -> String {
    let namespace = Sha256::digest(config.state_dir.to_string_lossy().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    format!("managed-{namespace}-identity-v1")
}
fn regular_file(path: &std::path::Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => Ok(true),
        Ok(_) => fail("managed state must be a regular file"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn reference(config: &SessionConfig) -> Result<Option<String>> {
    let path = config.state_dir.join(REFERENCE);
    if !regular_file(&path)? {
        return Ok(None);
    }
    let metadata = fs::metadata(&path)?;
    if metadata.len() > 256 {
        return fail("identity reference too large");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return fail("identity reference permissions must be 0600");
        }
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(257).read_to_end(&mut bytes)?;
    if bytes.len() > 256 {
        return fail("identity reference too large");
    }
    let locator: IdentityReference = serde_json::from_slice(&bytes)?;
    if locator.version != 1 || !valid_account(&locator.account) {
        return fail("invalid identity reference");
    }
    Ok(Some(locator.account))
}
pub(super) fn save_reference(config: &SessionConfig, account: &str) -> Result<()> {
    let temp = config
        .state_dir
        .join(format!(".identity-{}.tmp", random_id()));
    let result = (|| -> Result<()> {
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        checkpoint("reference_before_write")?;
        let mut file = opts.open(&temp)?;
        file.write_all(&serde_json::to_vec(&IdentityReference {
            version: 1,
            account: account.into(),
        })?)?;
        file.sync_all()?;
        checkpoint("reference_before_rename")?;
        fs::rename(&temp, config.state_dir.join(REFERENCE))?;
        checkpoint("reference_after_rename")?;
        fs::File::open(&config.state_dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result.map_err(|_| crate::Error::Managed {
        code: super::SessionErrorCode::StorageCommitUncertain,
        message: "identity reference commit could not be confirmed; reopen before retrying".into(),
    })
}
fn validate_envelope(
    envelope: &Envelope,
    identity: &Identity,
    manifest: &crate::AppManifest,
) -> Result<()> {
    if envelope.version != 1 {
        return fail("unsupported session journal version");
    }
    if &envelope.identity != identity || !envelope.metadata.manifest.compatible_with(manifest) {
        return fail("session identity or manifest changed");
    }
    envelope.metadata.validate()?;
    super::limits::state(envelope)?;
    AppKey::from_slice(&envelope.group_key)?;
    if let Some(prepared) = &envelope.prepared {
        validate_envelope(&prepared.target, identity, manifest)?;
    }
    Ok(())
}
pub(super) fn identity(
    config: &SessionConfig,
    store: &dyn KeyStore,
) -> Result<(Identity, DeviceKeys, AppKey, Option<String>)> {
    use super::{fail_code, SessionErrorCode};
    let locator = reference(config)?;
    let existing = regular_file(&config.state_dir.join("session.enc"))?;
    let candidate = config.legacy_identity_key.as_deref();
    if candidate.is_some() && (locator.is_some() || !existing) {
        return fail_code(
            SessionErrorCode::InvalidConfiguration,
            "legacy candidate requires existing state without an identity reference",
        );
    }
    if candidate.is_some_and(|s| !valid_account(s)) {
        return fail_code(
            SessionErrorCode::InvalidConfiguration,
            "invalid legacy identity account",
        );
    }
    let name = if let Some(account) = locator.as_deref().or(candidate) {
        account.to_owned()
    } else if existing {
        legacy_account(config)
    } else {
        use rand_core::{OsRng, RngCore};
        let mut bytes = [0u8; 32];
        OsRng.try_fill_bytes(&mut bytes).map_err(|e| {
            crate::Error::Crypto(format!("identity namespace entropy unavailable: {e}"))
        })?;
        format!(
            "managed-{}-identity-v1",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        )
    };
    let bundle: IdentityBundle = match store.get(&name)? {
        Some(bytes) => match serde_json::from_slice(&bytes) {
            Ok(bundle) => bundle,
            Err(_) if candidate.is_some() => {
                return fail_code(
                    SessionErrorCode::IdentityCandidateMismatch,
                    "legacy identity candidate does not match",
                )
            }
            Err(e) => return Err(e.into()),
        },
        None if candidate.is_some() => {
            return fail_code(
                SessionErrorCode::IdentityCandidateMismatch,
                "legacy identity candidate does not match",
            )
        }
        None if existing && locator.is_none() => {
            return fail_code(
                SessionErrorCode::IdentityMigrationRequired,
                "legacy identity migration required",
            )
        }
        None if locator.is_some() => {
            return fail("secure storage identity absent for existing reference")
        }
        None => {
            let identity = Identity::new(
                format!("device-{}", random_id()),
                &config.metadata.manifest.app_id,
                "managed",
            );
            let keys = DeviceKeys::generate(&identity)?;
            let bundle = IdentityBundle {
                identity,
                certificate: keys.cert_der().to_vec(),
                private_key: keys.key_der().to_vec(),
                storage_key: AppKey::generate()?.as_bytes().to_vec(),
            };
            store.set(&name, &serde_json::to_vec(&bundle)?)?;
            bundle
        }
    };
    let validate = || -> Result<(Identity, DeviceKeys, AppKey)> {
        if bundle.certificate.is_empty()
            || bundle.private_key.is_empty()
            || bundle.identity.app_id != config.metadata.manifest.app_id
        {
            return fail("incomplete secure storage identity");
        }
        let keys = DeviceKeys::from_der(bundle.certificate, bundle.private_key)?;
        crate::sync::validate_device_keys(&keys)?;
        let key = AppKey::from_slice(&bundle.storage_key)?;
        if existing {
            let envelope = load(config, &key)?
                .ok_or_else(|| crate::Error::Protocol("existing journal disappeared".into()))?;
            validate_envelope(&envelope, &bundle.identity, &config.metadata.manifest)?;
        }
        Ok((bundle.identity, keys, key))
    };
    let (identity, keys, key) = match validate() {
        Err(crate::Error::Io(e)) => return Err(e.into()),
        Err(_) if candidate.is_some() => {
            return fail_code(
                SessionErrorCode::IdentityCandidateMismatch,
                "legacy identity candidate does not match",
            )
        }
        result => result?,
    };
    if locator.is_none() && !existing {
        save_reference(config, &name)?;
    }
    Ok((
        identity,
        keys,
        key,
        if locator.is_none() && existing {
            Some(name)
        } else {
            None
        },
    ))
}
pub(super) fn load(config: &SessionConfig, key: &AppKey) -> Result<Option<Envelope>> {
    let bytes = match fs::read(config.state_dir.join("session.enc")) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let e: Envelope = serde_json::from_slice(&decrypt_blob(key, &bytes, &aad(config))?)?;
    if e.version != 1 {
        return fail("unsupported session journal version");
    }
    Ok(Some(e))
}
pub(super) fn save(config: &SessionConfig, key: &AppKey, envelope: &Envelope) -> Result<()> {
    fs::create_dir_all(&config.state_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&config.state_dir, fs::Permissions::from_mode(0o700))?;
    }
    let temp = config
        .state_dir
        .join(format!(".session-{}.tmp", random_id()));
    let result = (|| -> Result<()> {
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        checkpoint("before_write")?;
        let mut file = opts.open(&temp)?;
        let plain = serde_json::to_vec(envelope)?;
        let cipher = encrypt_blob(key, &plain, &aad(config))?;
        file.write_all(&cipher)?;
        checkpoint("after_write")?;
        file.sync_all()?;
        checkpoint("after_fsync")?;
        checkpoint("before_rename")?;
        fs::rename(&temp, config.state_dir.join("session.enc"))?;
        checkpoint("after_rename")?;
        fs::File::open(&config.state_dir)?.sync_all()?;
        checkpoint("after_dirsync")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

pub(super) fn lease(config: &SessionConfig) -> Result<crate::lease::StateLease> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&config.state_dir, fs::Permissions::from_mode(0o700))?;
    }
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    crate::lease::StateLease::acquire(
        options.open(config.state_dir.join("session.lock"))?,
        "managed state directory",
    )
}

#[cfg(not(test))]
fn checkpoint(_stage: &str) -> Result<()> {
    Ok(())
}
#[cfg(test)]
thread_local! { static FAILURE: std::cell::RefCell<Option<(String,usize)>> = const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
fn checkpoint(stage: &str) -> Result<()> {
    FAILURE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some((point, remaining)) = slot.as_mut() {
            if point == stage {
                if *remaining == 0 {
                    *slot = None;
                    return Err(std::io::Error::other(format!(
                        "injected storage failure at {stage}"
                    ))
                    .into());
                }
                *remaining -= 1;
            }
        }
        Ok(())
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AdapterDescriptor, AppManifest, DeviceMetadata, MemoryKeyStore, Session};
    use std::sync::Arc;
    fn config(path: &std::path::Path) -> SessionConfig {
        SessionConfig::new(
            path,
            DeviceMetadata {
                display_name: "test".into(),
                device_kind: "desktop".into(),
                role: "app".into(),
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
        )
    }
    #[derive(Default)]
    struct ObservedKeyStore {
        values: std::sync::Mutex<std::collections::BTreeMap<String, Vec<u8>>>,
        gets: std::sync::atomic::AtomicUsize,
        writes: std::sync::atomic::AtomicUsize,
    }
    impl ObservedKeyStore {
        fn reset_counts(&self) {
            self.gets.store(0, std::sync::atomic::Ordering::SeqCst);
            self.writes.store(0, std::sync::atomic::Ordering::SeqCst);
        }
    }
    impl KeyStore for ObservedKeyStore {
        fn get(&self, name: &str) -> Result<Option<Vec<u8>>> {
            self.gets.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self.values.lock().unwrap().get(name).cloned())
        }
        fn set(&self, name: &str, value: &[u8]) -> Result<()> {
            self.writes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.values
                .lock()
                .unwrap()
                .insert(name.into(), value.into());
            Ok(())
        }
        fn delete(&self, name: &str) -> Result<()> {
            self.writes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.values.lock().unwrap().remove(name);
            Ok(())
        }
    }
    fn state_entries(path: &std::path::Path) -> std::collections::BTreeSet<std::ffi::OsString> {
        fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect()
    }
    #[test]
    fn unsafe_identity_references_fail_before_secure_store_access_or_writes() {
        let mut cases = vec!["unsupported_version", "oversized"];
        #[cfg(unix)]
        cases.extend(["permissions", "symlink"]);
        for case in cases {
            let dir = tempfile::tempdir().unwrap();
            let keys = Arc::new(ObservedKeyStore::default());
            let c = config(dir.path());
            drop(Session::open(c.clone(), keys.clone()).unwrap());
            let path = dir.path().join(REFERENCE);
            let valid = fs::read(&path).unwrap();
            match case {
                "unsupported_version" => {
                    let mut value: serde_json::Value = serde_json::from_slice(&valid).unwrap();
                    value["version"] = serde_json::json!(2);
                    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
                }
                "oversized" => fs::write(&path, vec![b' '; 257]).unwrap(),
                #[cfg(unix)]
                "permissions" => {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
                }
                #[cfg(unix)]
                "symlink" => {
                    fs::rename(&path, dir.path().join("original-reference")).unwrap();
                    std::os::unix::fs::symlink("original-reference", &path).unwrap();
                }
                _ => unreachable!(),
            }
            let journal = fs::read(dir.path().join("session.enc")).unwrap();
            let reference_bytes = fs::read(&path).unwrap();
            let entries = state_entries(dir.path());
            let values = keys.values.lock().unwrap().clone();
            keys.reset_counts();
            assert!(Session::open(c, keys.clone()).is_err(), "{case}");
            assert_eq!(
                keys.gets.load(std::sync::atomic::Ordering::SeqCst),
                0,
                "{case}"
            );
            assert_eq!(
                keys.writes.load(std::sync::atomic::Ordering::SeqCst),
                0,
                "{case}"
            );
            assert_eq!(*keys.values.lock().unwrap(), values, "{case}");
            assert_eq!(
                fs::read(dir.path().join("session.enc")).unwrap(),
                journal,
                "{case}"
            );
            assert_eq!(fs::read(&path).unwrap(), reference_bytes, "{case}");
            assert_eq!(state_entries(dir.path()), entries, "{case}");
            #[cfg(unix)]
            if case == "symlink" {
                assert!(fs::symlink_metadata(path).unwrap().file_type().is_symlink());
            }
        }
    }
    #[test]
    fn authenticated_malformed_legacy_candidates_never_write_state_or_keys() {
        for case in [
            "envelope_json",
            "envelope_version",
            "certificate_key_mismatch",
            "identity",
            "manifest",
            "metadata",
            "limits",
            "prepared_target",
            "prepared_target_version",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let keys = Arc::new(ObservedKeyStore::default());
            let mut c = config(dir.path());
            let session = Session::open(c.clone(), keys.clone()).unwrap();
            let storage_key = session.inner.storage_key.clone();
            let mut envelope = session.inner.envelope.lock().unwrap().clone();
            drop(session);
            let account = reference(&c).unwrap().unwrap();
            fs::remove_file(dir.path().join(REFERENCE)).unwrap();
            c.legacy_identity_key = Some(account.clone());
            match case {
                "envelope_json" => {
                    fs::write(
                        dir.path().join("session.enc"),
                        encrypt_blob(&storage_key, b"authenticated invalid json", &aad(&c))
                            .unwrap(),
                    )
                    .unwrap();
                }
                "certificate_key_mismatch" => {
                    let mut bundle: IdentityBundle =
                        serde_json::from_slice(&keys.get(&account).unwrap().unwrap()).unwrap();
                    bundle.private_key = DeviceKeys::generate(&bundle.identity)
                        .unwrap()
                        .key_der()
                        .to_vec();
                    keys.set(&account, &serde_json::to_vec(&bundle).unwrap())
                        .unwrap();
                }
                _ => {
                    match case {
                        "envelope_version" => envelope.version = 2,
                        "identity" => envelope.identity.device_id = "other-device".into(),
                        "manifest" => envelope.metadata.manifest.app_id = "other.app".into(),
                        "metadata" => envelope.metadata.display_name = "invalid\nname".into(),
                        "limits" => envelope.epoch.clear(),
                        "prepared_target" | "prepared_target_version" => {
                            let mut target = envelope.clone();
                            if case == "prepared_target_version" {
                                target.version = 2;
                            } else {
                                target.metadata.display_name = "invalid\nname".into();
                            }
                            envelope.prepared = Some(super::super::PreparedTransaction {
                                id: "invalid-target".into(),
                                changes: vec![],
                                target: Box::new(target),
                            });
                        }
                        _ => unreachable!(),
                    }
                    save(&c, &storage_key, &envelope).unwrap();
                }
            }
            let journal = fs::read(dir.path().join("session.enc")).unwrap();
            let entries = state_entries(dir.path());
            let values = keys.values.lock().unwrap().clone();
            keys.reset_counts();
            assert!(
                matches!(
                    Session::open(c, keys.clone()),
                    Err(crate::Error::Managed {
                        code: super::super::SessionErrorCode::IdentityCandidateMismatch,
                        ..
                    })
                ),
                "{case}"
            );
            assert_eq!(
                keys.gets.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "{case}"
            );
            assert_eq!(
                keys.writes.load(std::sync::atomic::Ordering::SeqCst),
                0,
                "{case}"
            );
            assert_eq!(*keys.values.lock().unwrap(), values, "{case}");
            assert_eq!(
                fs::read(dir.path().join("session.enc")).unwrap(),
                journal,
                "{case}"
            );
            assert!(!dir.path().join(REFERENCE).exists(), "{case}");
            assert_eq!(state_entries(dir.path()), entries, "{case}");
        }
    }
    #[test]
    fn locator_commit_failures_never_publish_first_journal_and_retry_safely() {
        for stage in [
            "reference_before_write",
            "reference_before_rename",
            "reference_after_rename",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let keys = Arc::new(MemoryKeyStore::new());
            FAILURE.with(|f| *f.borrow_mut() = Some((stage.into(), 0)));
            assert!(Session::open(config(dir.path()), keys.clone()).is_err());
            assert!(!dir.path().join("session.enc").exists());
            let committed_reference = reference(&config(dir.path())).unwrap();
            let s = Session::open(config(dir.path()), keys.clone()).unwrap();
            assert!(dir.path().join("session.enc").is_file());
            if let Some(account) = committed_reference {
                assert_eq!(reference(&config(dir.path())).unwrap(), Some(account));
            }
            let identity = s.snapshot().unwrap().identity;
            drop(s);
            assert_eq!(
                Session::open(config(dir.path()), keys)
                    .unwrap()
                    .snapshot()
                    .unwrap()
                    .identity,
                identity
            );
        }
    }
    #[test]
    fn legacy_locator_is_persisted_only_after_successful_journal_save() {
        let dir = tempfile::tempdir().unwrap();
        let keys = Arc::new(MemoryKeyStore::new());
        let c = config(dir.path());
        let s = Session::open(c.clone(), keys.clone()).unwrap();
        let identity = s.snapshot().unwrap().identity;
        drop(s);
        let account = reference(&c).unwrap().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        let mut canonical_config = config(&canonical);
        let old = legacy_account(&canonical_config);
        keys.set(&old, &keys.get(&account).unwrap().unwrap())
            .unwrap();
        keys.delete(&account).unwrap();
        fs::remove_file(dir.path().join(REFERENCE)).unwrap();
        let before = fs::read(dir.path().join("session.enc")).unwrap();
        FAILURE.with(|f| *f.borrow_mut() = Some(("before_write".into(), 0)));
        assert!(Session::open(c.clone(), keys.clone()).is_err());
        assert!(reference(&c).unwrap().is_none());
        assert_eq!(fs::read(dir.path().join("session.enc")).unwrap(), before);
        canonical_config.legacy_identity_key = Some(old.clone());
        FAILURE.with(|f| *f.borrow_mut() = Some(("reference_before_rename".into(), 0)));
        assert!(Session::open(canonical_config.clone(), keys.clone()).is_err());
        assert!(reference(&c).unwrap().is_none());
        assert_eq!(
            Session::open(canonical_config, keys)
                .unwrap()
                .snapshot()
                .unwrap()
                .identity,
            identity
        );
        assert_eq!(reference(&c).unwrap(), Some(old));
    }

    #[test]
    fn empty_name_is_generated_once_and_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let keys = Arc::new(MemoryKeyStore::new());
        let mut c = config(dir.path());
        c.metadata.display_name.clear();
        c.advertise = false;
        c.listen = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
        // The empty-name shorthand belongs to SessionConfig, never wire metadata.
        assert!(c.metadata.validate().is_err());
        let session = Session::open(c.clone(), keys.clone()).unwrap();
        let name = session.inner.config.metadata.display_name.clone();
        session.inner.config.metadata.validate().unwrap();
        assert_eq!(name.split_whitespace().count(), 2);
        assert_eq!(
            session.inner.envelope.lock().unwrap().metadata.display_name,
            name
        );
        let identity = session.snapshot().unwrap().identity;
        session.set("records", "saved", b"value".to_vec()).unwrap();
        drop(session);
        let reopened = Session::open(c, keys).unwrap();
        assert_eq!(reopened.inner.config.metadata.display_name, name);
        assert_eq!(reopened.snapshot().unwrap().identity, identity);
        reopened.start().unwrap();
        assert_eq!(reopened.platform_advertisement().unwrap().txt["name"], name);
        let invitation = reopened
            .create_invitation(std::time::Duration::from_secs(30))
            .unwrap();
        assert_eq!(invitation.invitation.metadata.display_name, name);
        reopened.shutdown().unwrap();
        assert_eq!(
            reopened.get("records", "saved").unwrap(),
            Some(b"value".to_vec())
        );
        let bytes = std::fs::read(dir.path().join("session.enc")).unwrap();
        assert!(!bytes.windows(name.len()).any(|w| w == name.as_bytes()));
    }

    #[test]
    fn independent_installations_have_independent_identities() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let keys = Arc::new(MemoryKeyStore::new());
        let mut ac = config(a.path());
        let mut bc = config(b.path());
        ac.metadata.display_name.clear();
        bc.metadata.display_name.clear();
        let a = Session::open(ac, keys.clone()).unwrap();
        let b = Session::open(bc, keys).unwrap();
        assert_ne!(
            a.snapshot().unwrap().identity,
            b.snapshot().unwrap().identity
        );
        a.inner.config.metadata.validate().unwrap();
        b.inner.config.metadata.validate().unwrap();
    }

    #[test]
    fn explicit_name_remains_authoritative_and_is_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let keys = Arc::new(MemoryKeyStore::new());
        let c = config(dir.path());
        drop(Session::open(c.clone(), keys.clone()).unwrap());
        let mut renamed = c.clone();
        renamed.metadata.display_name = "Caller supplied".into();
        let session = Session::open(renamed, keys.clone()).unwrap();
        assert_eq!(
            session.inner.config.metadata.display_name,
            "Caller supplied"
        );
        assert_eq!(
            session.inner.envelope.lock().unwrap().metadata.display_name,
            "Caller supplied"
        );
        drop(session);
        let mut automatic = c;
        automatic.metadata.display_name.clear();
        let reopened = Session::open(automatic, keys).unwrap();
        assert_eq!(
            reopened.inner.config.metadata.display_name,
            "Caller supplied"
        );
    }

    #[test]
    fn empty_name_does_not_bypass_other_metadata_validation() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["invalid\nname".to_string(), "x".repeat(129)] {
            let mut c = config(dir.path());
            c.metadata.display_name = name;
            assert!(Session::open(c, Arc::new(MemoryKeyStore::new())).is_err());
        }
        let mut c = config(dir.path());
        c.metadata.display_name.clear();
        c.metadata.device_kind.clear();
        assert!(Session::open(c, Arc::new(MemoryKeyStore::new())).is_err());
        assert!(!dir.path().join("session.enc").exists());
    }

    #[test]
    fn generated_name_follows_recovered_prepared_target() {
        let dir = tempfile::tempdir().unwrap();
        let keys = Arc::new(MemoryKeyStore::new());
        let mut c = config(dir.path());
        c.metadata.display_name.clear();
        let session = Session::open(c.clone(), keys.clone()).unwrap();
        // Simulate an interrupted transaction with a valid authoritative target.
        let mut journal = session.inner.envelope.lock().unwrap().clone();
        let mut target = journal.clone();
        target.metadata.display_name = "Recovered Avocado".into();
        journal.prepared = Some(super::super::PreparedTransaction {
            id: "recover-name".into(),
            changes: Vec::new(),
            target: Box::new(target),
        });
        save(&session.inner.config, &session.inner.storage_key, &journal).unwrap();
        drop(session);
        let reopened = Session::open(c, keys).unwrap();
        assert_eq!(
            reopened.inner.config.metadata.display_name,
            "Recovered Avocado"
        );
        assert_eq!(
            reopened
                .inner
                .envelope
                .lock()
                .unwrap()
                .metadata
                .display_name,
            "Recovered Avocado"
        );
    }

    #[test]
    fn session_owner_drop_releases_lease_with_retained_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let keys = Arc::new(MemoryKeyStore::new());
        let session = Session::open(config(dir.path()), keys.clone()).unwrap();
        let inherited = session.inner._lease.try_clone().unwrap();
        assert!(matches!(
            Session::open(config(dir.path()), keys.clone()),
            Err(crate::Error::Managed {
                code: super::super::SessionErrorCode::Busy,
                ..
            })
        ));
        drop(session);
        let reopened = Session::open(config(dir.path()), keys.clone()).unwrap();
        assert!(Session::open(config(dir.path()), keys.clone()).is_err());
        drop(reopened);
        // Failed identity recovery must also release the newly acquired lease.
        assert!(Session::open(config(dir.path()), Arc::new(MemoryKeyStore::new())).is_err());
        let recovered = Session::open(config(dir.path()), keys).unwrap();
        drop(recovered);
        drop(inherited);
    }

    #[test]
    fn queued_mutation_cannot_write_after_storage_commit_becomes_uncertain() {
        let dir = tempfile::tempdir().unwrap();
        let session =
            Arc::new(Session::open(config(dir.path()), Arc::new(MemoryKeyStore::new())).unwrap());
        let held = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let waiting = Arc::new(std::sync::Barrier::new(2));
        let writer_session = session.clone();
        let writer_held = held.clone();
        let writer_release = release.clone();
        let writer = std::thread::spawn(move || {
            FAILURE.with(|f| *f.borrow_mut() = Some(("after_fsync".into(), 0)));
            writer_session.inner.mutate(|e| {
                writer_held.wait();
                writer_release.wait();
                e.revision += 1;
                Ok(())
            })
        });
        held.wait();
        let queued_session = session.clone();
        let queued_waiting = waiting.clone();
        let published = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let queued_published = published.clone();
        let queued = std::thread::spawn(move || {
            super::super::MUTATION_BEFORE_LOCK
                .with(|slot| *slot.borrow_mut() = Some(queued_waiting));
            queued_session.inner.mutate(|e| {
                queued_published.store(true, std::sync::atomic::Ordering::SeqCst);
                e.revision += 10;
                Ok(())
            })
        });
        waiting.wait();
        release.wait();
        assert!(writer.join().unwrap().is_err());
        assert!(
            queued.join().unwrap().is_err(),
            "queued mutation wrote after failure"
        );
        assert!(!published.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(session.inner.envelope.lock().unwrap().revision, 0);
        assert!(matches!(
            session.snapshot().unwrap().phase,
            super::super::SessionPhase::Failed
        ));
    }
    #[test]
    fn every_atomic_write_boundary_recovers_without_publishing_failed_candidate() {
        for final_save in [false, true] {
            for stage in [
                "before_write",
                "after_write",
                "after_fsync",
                "before_rename",
                "after_rename",
                "after_dirsync",
            ] {
                let dir = tempfile::tempdir().unwrap();
                let keys = Arc::new(MemoryKeyStore::new());
                let session = Session::open(config(dir.path()), keys.clone()).unwrap();
                session.set("records", "x", b"before".to_vec()).unwrap();
                FAILURE.with(|f| *f.borrow_mut() = Some((stage.into(), usize::from(final_save))));
                assert!(
                    session.set("records", "x", b"after".to_vec()).is_err(),
                    "{stage}"
                );
                assert_eq!(
                    session.get("records", "x").unwrap(),
                    Some(b"before".to_vec())
                );
                assert!(matches!(
                    session.snapshot().unwrap().phase,
                    super::super::SessionPhase::Failed
                ));
                drop(session);
                let reopened = Session::open(config(dir.path()), keys).unwrap();
                let committed = final_save || matches!(stage, "after_rename" | "after_dirsync");
                assert_eq!(
                    reopened.get("records", "x").unwrap(),
                    Some(if committed {
                        b"after".to_vec()
                    } else {
                        b"before".to_vec()
                    }),
                    "{stage}; final={final_save}"
                );
            }
        }
    }
    #[test]
    fn changed_certificate_preserves_data_and_requires_explicit_pinned_repair() {
        let ad = tempfile::tempdir().unwrap();
        let bd = tempfile::tempdir().unwrap();
        let ak = Arc::new(MemoryKeyStore::new());
        let mut ac = config(ad.path());
        ac.advertise = false;
        ac.listen = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
        let mut bc = config(bd.path());
        bc.advertise = false;
        bc.listen = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
        let a = Session::open(ac.clone(), ak.clone()).unwrap();
        let b = Session::open(bc, Arc::new(MemoryKeyStore::new())).unwrap();
        a.start().unwrap();
        b.start().unwrap();
        b.connect(
            &a.create_invitation(std::time::Duration::from_secs(30))
                .unwrap(),
        )
        .unwrap();
        let aid = a.snapshot().unwrap().identity.device_id;
        b.set("records", "local", b"preserved".to_vec()).unwrap();
        a.shutdown().unwrap();
        drop(a);
        let locator: serde_json::Value =
            serde_json::from_slice(&std::fs::read(ad.path().join("identity-ref.json")).unwrap())
                .unwrap();
        let name = locator["account"].as_str().unwrap().to_owned();
        let mut bundle: IdentityBundle =
            serde_json::from_slice(&ak.get(&name).unwrap().unwrap()).unwrap();
        let keys = DeviceKeys::generate(&bundle.identity).unwrap();
        bundle.certificate = keys.cert_der().to_vec();
        bundle.private_key = keys.key_der().to_vec();
        ak.set(&name, &serde_json::to_vec(&bundle).unwrap())
            .unwrap();
        let a = Session::open(ac, ak).unwrap();
        a.start().unwrap();
        b.inner
            .mutate(|e| {
                e.peers.get_mut(&aid).unwrap().addresses = vec![a.address().unwrap()];
                Ok(())
            })
            .unwrap();
        b.wake().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        while b.snapshot().unwrap().peers[0].state != super::super::PeerState::NeedsRepair {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(b
            .connect(
                &a.create_invitation(std::time::Duration::from_secs(30))
                    .unwrap()
            )
            .is_err());
        assert_eq!(
            b.get("records", "local").unwrap(),
            Some(b"preserved".to_vec())
        );
        b.repair_peer(
            &aid,
            &a.create_invitation(std::time::Duration::from_secs(30))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            b.snapshot().unwrap().peers[0].fingerprint,
            keys.fingerprint()
        );
        assert_eq!(
            b.get("records", "local").unwrap(),
            Some(b"preserved".to_vec())
        );
    }
}
