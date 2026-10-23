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
pub(super) fn identity(
    config: &SessionConfig,
    store: &dyn KeyStore,
) -> Result<(Identity, DeviceKeys, AppKey)> {
    let namespace = Sha256::digest(config.state_dir.to_string_lossy().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let name = format!("managed-{namespace}-identity-v1");
    let bundle: IdentityBundle = match store.get(&name)? {
        Some(bytes) => serde_json::from_slice(&bytes)?,
        None => {
            if config.state_dir.join("session.enc").exists() {
                return fail("secure storage identity absent for existing state");
            }
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
    if bundle.certificate.is_empty()
        || bundle.private_key.is_empty()
        || bundle.identity.app_id != config.metadata.manifest.app_id
    {
        return fail("incomplete secure storage identity");
    }
    let keys = DeviceKeys::from_der(bundle.certificate, bundle.private_key)?;
    crate::sync::validate_device_keys(&keys)?;
    Ok((
        bundle.identity,
        keys,
        AppKey::from_slice(&bundle.storage_key)?,
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
        let namespace = Sha256::digest(
            std::fs::canonicalize(ad.path())
                .unwrap()
                .to_string_lossy()
                .as_bytes(),
        )
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
        let name = format!("managed-{namespace}-identity-v1");
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
