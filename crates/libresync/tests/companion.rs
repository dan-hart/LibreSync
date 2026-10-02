use libresync::{
    companion::{momentum_manifest, CompanionManager, MomentumAdapter},
    DeviceMetadata, ManagedAdapter, MemoryKeyStore, Session, SessionConfig, SyncRecord,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
fn fixtures() -> Vec<SyncRecord> {
    serde_json::from_value(
        serde_json::from_str::<serde_json::Value>(include_str!("fixtures/momentum-766064b.json"))
            .unwrap()["records"]
            .clone(),
    )
    .unwrap()
}
fn wait(mut test: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(12);
    while !test() {
        assert!(Instant::now() < end, "sync timeout");
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn app(path: &std::path::Path) -> Session {
    let mut c = SessionConfig::new(
        path,
        DeviceMetadata {
            display_name: "Momentum phone".into(),
            device_kind: "phone".into(),
            role: "application".into(),
            manifest: momentum_manifest(),
        },
    );
    c.advertise = false;
    c.catch_up = Duration::from_millis(100);
    Session::open(
        c.with_adapter(Arc::new(MomentumAdapter)).unwrap(),
        Arc::new(MemoryKeyStore::new()),
    )
    .unwrap()
}
#[test]
fn real_momentum_wire_roundtrip_collision_and_tombstone() {
    let adapter = MomentumAdapter;
    let raw = fixtures();
    let records = raw
        .iter()
        .map(MomentumAdapter::encode)
        .collect::<libresync::Result<Vec<_>>>()
        .unwrap();
    for (wire, original) in records.iter().zip(&raw) {
        assert_eq!(MomentumAdapter::decode(wire).unwrap(), *original);
    }
    let staged = adapter.prepare(&[], &records, 0).unwrap();
    assert_eq!(staged.staged.len(), 3);
    let mut changed = raw[0].clone();
    changed
        .fields
        .insert("action".into(), libresync::FieldValue::String("{}".into()));
    changed.clock.counter += 1;
    assert!(adapter
        .prepare(
            &staged.staged,
            &[MomentumAdapter::encode(&changed).unwrap()],
            1
        )
        .is_err());
    let mut deleted = raw[0].clone();
    deleted.tombstone = true;
    deleted.fields.clear();
    deleted.field_clocks.clear();
    deleted.clock.counter += 10;
    let out = adapter
        .prepare(
            &staged.staged,
            &[MomentumAdapter::encode(&deleted).unwrap()],
            1,
        )
        .unwrap();
    assert!(
        out.staged
            .iter()
            .find(|r| r.id == records[0].id)
            .unwrap()
            .deleted
    );
    assert!(
        adapter
            .prepare(&out.staged, &[records[0].clone()], 2)
            .unwrap()
            .staged
            .iter()
            .find(|r| r.id == records[0].id)
            .unwrap()
            .deleted
    );
}
#[test]
fn actual_momentum_stored_restart_forward_then_app_applied_and_group_isolation() {
    let root = tempfile::tempdir().unwrap();
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(MemoryKeyStore::new());
    let a = app(a_dir.path());
    a.start().unwrap();
    let records = fixtures()
        .iter()
        .map(MomentumAdapter::encode)
        .collect::<libresync::Result<Vec<_>>>()
        .unwrap();
    a.import_records(&records).unwrap();
    let mut manager = CompanionManager::open(root.path(), "My Mac", keys.clone(), false).unwrap();
    let space = manager
        .enroll(&a.create_invitation(Duration::from_secs(120)).unwrap())
        .unwrap();
    wait(|| {
        manager
            .session(&space)
            .unwrap()
            .records("ops")
            .unwrap()
            .len()
            == 3
    });
    let aid = a.snapshot().unwrap().identity.device_id;
    wait(|| {
        a.snapshot()
            .unwrap()
            .peers
            .iter()
            .any(|p| p.stored.sequence > 0)
    });
    assert_eq!(a.snapshot().unwrap().peers[0].applied.sequence, 0);
    let second = manager.add_app(momentum_manifest()).unwrap();
    assert!(manager
        .session(&second)
        .unwrap()
        .records("ops")
        .unwrap()
        .is_empty());
    let sid = manager
        .session(&space)
        .unwrap()
        .snapshot()
        .unwrap()
        .identity;
    manager.pause(&second).unwrap();
    a.shutdown().unwrap();
    drop(manager);
    let mut manager = CompanionManager::open(root.path(), "My Mac", keys, false).unwrap();
    assert_eq!(
        manager
            .session(&space)
            .unwrap()
            .snapshot()
            .unwrap()
            .identity,
        sid
    );
    assert_eq!(
        manager.session(&second).unwrap().snapshot().unwrap().phase,
        libresync::SessionPhase::Paused
    );
    let b = app(b_dir.path());
    b.start().unwrap();
    b.connect(
        &manager
            .session(&space)
            .unwrap()
            .create_invitation(Duration::from_secs(120))
            .unwrap(),
    )
    .unwrap();
    wait(|| b.records("ops").unwrap().len() == 3);
    assert_eq!(
        b.records("ops").unwrap(),
        manager.session(&space).unwrap().records("ops").unwrap()
    );
    let inbox = b.application_inbox().unwrap();
    assert!(inbox.receipts.contains_key(&sid.device_id));
    b.acknowledge_inbox(&inbox).unwrap();
    wait(|| {
        manager
            .session(&space)
            .unwrap()
            .snapshot()
            .unwrap()
            .peers
            .iter()
            .any(|p| {
                p.identity.device_id == b.snapshot().unwrap().identity.device_id
                    && p.applied.sequence > 0
            })
    });
    assert!(manager
        .session(&space)
        .unwrap()
        .snapshot()
        .unwrap()
        .peers
        .iter()
        .any(|p| p.identity.device_id == aid));
    let mut bad = momentum_manifest();
    bad.adapters[0].schema = "json-file".into();
    assert!(manager.add_app(bad).is_err());
}
#[test]
fn encrypted_backups_are_bounded_export_only_and_pause_remove_persist() {
    let root = tempfile::tempdir().unwrap();
    let keys = Arc::new(MemoryKeyStore::new());
    let mut manager = CompanionManager::open(root.path(), "Mac", keys.clone(), false).unwrap();
    let id = manager.add_app(momentum_manifest()).unwrap();
    let records = fixtures()
        .iter()
        .map(MomentumAdapter::encode)
        .collect::<libresync::Result<Vec<_>>>()
        .unwrap();
    manager
        .session(&id)
        .unwrap()
        .import_records(&records)
        .unwrap();
    let mut latest = String::new();
    for _ in 0..7 {
        latest = manager.backup(&id).unwrap();
    }
    assert_eq!(manager.backups(&id).unwrap().len(), 5);
    let path = root.path().join("backups").join(&id).join(&latest);
    let cipher = std::fs::read(path).unwrap();
    assert!(!String::from_utf8_lossy(&cipher).contains("Local sync example"));
    assert_eq!(
        manager.export_backup(&id, &latest).unwrap().records.len(),
        3
    );
    assert!(manager.export_backup(&id, "../../spaces.json").is_err());
    manager.pause(&id).unwrap();
    assert_eq!(
        manager.session(&id).unwrap().snapshot().unwrap().phase,
        libresync::SessionPhase::Paused
    );
    manager.resume(&id).unwrap();
    assert_eq!(
        manager.session(&id).unwrap().snapshot().unwrap().phase,
        libresync::SessionPhase::Running
    );
    manager.remove(&id).unwrap();
    drop(manager);
    let manager = CompanionManager::open(root.path(), "Mac", keys, false).unwrap();
    assert!(manager.snapshots().unwrap().is_empty());
    assert!(root
        .path()
        .join("spaces")
        .join(&id)
        .join("session.enc")
        .exists());
}
#[test]
fn snapshot_compressed_expanded_limits_and_offline_qr() {
    let raw = fixtures();
    let snapshot = &raw[2];
    let encoded = match &snapshot.fields["gz_b64"] {
        libresync::FieldValue::String(s) => s,
        _ => panic!(),
    };
    assert!(MomentumAdapter::validate_snapshot(encoded, 1, 1024 * 1024).is_err());
    assert!(MomentumAdapter::validate_snapshot(encoded, 1024 * 1024, 1).is_err());
    assert!(MomentumAdapter::validate_snapshot("invalid", 1024, 1024).is_err());
    let root = tempfile::tempdir().unwrap();
    let mut manager =
        CompanionManager::open(root.path(), "Mac", Arc::new(MemoryKeyStore::new()), false).unwrap();
    let id = manager.add_app(momentum_manifest()).unwrap();
    let invitation = manager
        .session(&id)
        .unwrap()
        .create_invitation(Duration::from_secs(120))
        .unwrap();
    let qr = libresync::companion::invitation_qr(&invitation).unwrap();
    assert!(qr.contains("<svg"));
    assert!(!qr.contains(&invitation.invitation.secret));
}
#[test]
fn legacy_migration_archives_copies_without_network_trust_or_source_changes() {
    let root = tempfile::tempdir().unwrap();
    let old = tempfile::tempdir().unwrap();
    let config = old.path().join("config.json");
    let data = old.path().join("libresync.json");
    let state = old.path().join("state.json");
    std::fs::write(&data, b"private old data").unwrap();
    std::fs::write(&state, b"encrypted old journal").unwrap();
    let bytes=serde_json::to_vec(&serde_json::json!({"app_id":"com.codedbydan.libresync","data_path":data,"state_path":state,"auto_accept_linking":true,"auto_approve_linking":true})).unwrap();
    std::fs::write(&config, &bytes).unwrap();
    let mut manager =
        CompanionManager::open(root.path(), "Mac", Arc::new(MemoryKeyStore::new()), false).unwrap();
    let archive = manager.archive_legacy(&config).unwrap();
    assert_eq!(std::fs::read(&config).unwrap(), bytes);
    assert_eq!(
        std::fs::read(archive.join("libresync.json")).unwrap(),
        b"private old data"
    );
    assert!(manager.snapshots().unwrap().is_empty());
    assert!(archive.join("config.json").exists());
    assert!(archive.join("migration.json").exists());
}
#[test]
fn two_different_registered_apps_keep_distinct_identity_data_and_keys_on_restart() {
    let root = tempfile::tempdir().unwrap();
    let keys = Arc::new(MemoryKeyStore::new());
    let mut manager = CompanionManager::open(root.path(), "Mac", keys.clone(), false).unwrap();
    let momentum = manager.add_app(momentum_manifest()).unwrap();
    let notes = manager
        .add_app(libresync::companion::notes_manifest())
        .unwrap();
    let records = fixtures()
        .iter()
        .map(MomentumAdapter::encode)
        .collect::<libresync::Result<Vec<_>>>()
        .unwrap();
    manager
        .session(&momentum)
        .unwrap()
        .import_records(&records)
        .unwrap();
    manager
        .session(&notes)
        .unwrap()
        .set("records", "note", b"Notes only".to_vec())
        .unwrap();
    let m = manager
        .session(&momentum)
        .unwrap()
        .snapshot()
        .unwrap()
        .identity;
    let n = manager
        .session(&notes)
        .unwrap()
        .snapshot()
        .unwrap()
        .identity;
    assert_ne!(m.app_id, n.app_id);
    assert_ne!(m.device_id, n.device_id);
    assert!(manager
        .session(&momentum)
        .unwrap()
        .get("records", "note")
        .is_err());
    drop(manager);
    let manager = CompanionManager::open(root.path(), "Mac", keys, false).unwrap();
    assert_eq!(
        manager
            .session(&momentum)
            .unwrap()
            .snapshot()
            .unwrap()
            .identity,
        m
    );
    assert_eq!(
        manager
            .session(&notes)
            .unwrap()
            .snapshot()
            .unwrap()
            .identity,
        n
    );
    assert_eq!(
        manager
            .session(&notes)
            .unwrap()
            .get("records", "note")
            .unwrap(),
        Some(b"Notes only".to_vec())
    );
    assert_eq!(
        manager
            .session(&momentum)
            .unwrap()
            .records("ops")
            .unwrap()
            .len(),
        3
    );
    assert!(manager
        .session(&notes)
        .unwrap()
        .snapshot()
        .unwrap()
        .peers
        .is_empty());
}
#[test]
fn failed_registry_publication_keeps_pause_resume_remove_coherent() {
    let root = tempfile::tempdir().unwrap();
    let mut manager =
        CompanionManager::open(root.path(), "Mac", Arc::new(MemoryKeyStore::new()), false).unwrap();
    let id = manager.add_app(momentum_manifest()).unwrap();
    std::fs::rename(
        root.path().join("spaces.json"),
        root.path().join("saved.json"),
    )
    .unwrap();
    std::fs::create_dir(root.path().join("spaces.json")).unwrap();
    assert!(manager.pause(&id).is_err());
    assert_eq!(
        manager.session(&id).unwrap().snapshot().unwrap().phase,
        libresync::SessionPhase::Running
    );
    assert!(!manager.snapshots().unwrap()[0].space.paused);
    assert!(manager.remove(&id).is_err());
    assert!(manager.session(&id).is_ok());
    std::fs::remove_dir(root.path().join("spaces.json")).unwrap();
    std::fs::rename(
        root.path().join("saved.json"),
        root.path().join("spaces.json"),
    )
    .unwrap();
    manager.pause(&id).unwrap();
    std::fs::rename(
        root.path().join("spaces.json"),
        root.path().join("saved.json"),
    )
    .unwrap();
    std::fs::create_dir(root.path().join("spaces.json")).unwrap();
    assert!(manager.resume(&id).is_err());
    assert_eq!(
        manager.session(&id).unwrap().snapshot().unwrap().phase,
        libresync::SessionPhase::Paused
    );
}
struct SelectiveKeys {
    inner: MemoryKeyStore,
    unavailable: std::sync::atomic::AtomicBool,
    absent: std::sync::atomic::AtomicBool,
}
impl libresync::KeyStore for SelectiveKeys {
    fn get(&self, name: &str) -> libresync::Result<Option<Vec<u8>>> {
        if name.ends_with("backup-key-v1") {
            if self.unavailable.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(libresync::Error::Protocol("keychain locked".into()));
            }
            if self.absent.load(std::sync::atomic::Ordering::SeqCst) {
                return Ok(None);
            }
        }
        libresync::KeyStore::get(&self.inner, name)
    }
    fn set(&self, name: &str, value: &[u8]) -> libresync::Result<()> {
        libresync::KeyStore::set(&self.inner, name, value)
    }
    fn delete(&self, name: &str) -> libresync::Result<()> {
        libresync::KeyStore::delete(&self.inner, name)
    }
}
#[test]
fn backup_missing_locked_or_corrupt_key_preserves_existing_exports() {
    let root = tempfile::tempdir().unwrap();
    let keys = Arc::new(SelectiveKeys {
        inner: MemoryKeyStore::new(),
        unavailable: false.into(),
        absent: false.into(),
    });
    let mut manager = CompanionManager::open(root.path(), "Mac", keys.clone(), false).unwrap();
    let id = manager.add_app(momentum_manifest()).unwrap();
    let first = manager.backup(&id).unwrap();
    keys.absent.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(manager.backup(&id).is_err());
    assert_eq!(manager.backups(&id).unwrap(), vec![first.clone()]);
    keys.absent
        .store(false, std::sync::atomic::Ordering::SeqCst);
    keys.unavailable
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(manager.backup(&id).is_err());
    assert!(manager.export_backup(&id, &first).is_err());
    keys.unavailable
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(manager.export_backup(&id, &first).is_ok());
    libresync::KeyStore::set(keys.as_ref(), &format!("{id}-backup-key-v1"), b"bad").unwrap();
    assert!(manager.backup(&id).is_err());
    assert_eq!(manager.backups(&id).unwrap(), vec![first]);
}
#[cfg(unix)]
#[test]
fn real_os_cli_backend_distinguishes_absent_locked_failed_and_corrupt_secrets() {
    use libresync::KeyStore;
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("backend");
    std::fs::write(&script,"#!/bin/sh\ncase \"$4\" in\nmissing) exit 44;;\nlocked) echo 'Keychain locked' >&2; exit 36;;\ncorrupt) echo 'invalid'; exit 0;;\nempty) exit 0;;\nesac\nexit 36\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    // security args put the account in argument 5, so tailor one fixed result per executable.
    for (name, body) in [
        ("missing", "exit 44"),
        ("locked", "echo locked >&2; exit 36"),
        ("corrupt", "echo invalid; exit 0"),
        ("empty", "exit 0"),
    ] {
        let file = root.path().join(name);
        std::fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        let store = libresync::SecurityCliKeyStore::new("test").with_command(file);
        let value = store.get("secret");
        if name == "missing" {
            assert_eq!(value.unwrap(), None)
        } else {
            assert!(value.is_err(), "{name} must not mean absent")
        }
    }
    for (name, body) in [
        ("notfound", "exit 1"),
        ("locked", "echo locked >&2; exit 1"),
        ("failed", "exit 2"),
        ("corrupt", "echo invalid; exit 0"),
        ("empty", "exit 0"),
    ] {
        let file = root.path().join(format!("secret-{name}"));
        std::fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        let store = libresync::SecretToolKeyStore::new("test").with_command(file);
        let value = store.get("secret");
        if name == "notfound" {
            assert_eq!(value.unwrap(), None)
        } else {
            assert!(value.is_err(), "{name} must not mean absent")
        }
    }
}
#[test]
fn caller_registered_exact_policy_is_required_again_on_restart() {
    use libresync::companion::{default_catalog, AppSupport};
    let root = tempfile::tempdir().unwrap();
    let keys = Arc::new(MemoryKeyStore::new());
    let mut manifest = libresync::companion::notes_manifest();
    manifest.app_id = "io.test.Sketches".into();
    manifest.display_name = "Sketches".into();
    manifest.adapters[0].namespace = "io.test.Sketches".into();
    let catalog = || {
        let mut list = default_catalog();
        list.push(AppSupport {
            manifest: manifest.clone(),
            adapters: vec![Arc::new(libresync::RecordsAdapter::new(
                manifest.adapters[0].clone(),
            ))],
        });
        list
    };
    let mut manager =
        CompanionManager::open_with_catalog(root.path(), "Mac", keys.clone(), false, catalog())
            .unwrap();
    let id = manager.add_app(manifest.clone()).unwrap();
    manager
        .session(&id)
        .unwrap()
        .set("records", "drawing", b"Sketch only".to_vec())
        .unwrap();
    drop(manager);
    assert!(CompanionManager::open(root.path(), "Mac", keys.clone(), false).is_err());
    let manager =
        CompanionManager::open_with_catalog(root.path(), "Mac", keys, false, catalog()).unwrap();
    assert_eq!(
        manager
            .session(&id)
            .unwrap()
            .get("records", "drawing")
            .unwrap(),
        Some(b"Sketch only".to_vec())
    );
}
#[test]
fn unrelated_tauri_config_is_not_offered_as_libresync_legacy_migration() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.json");
    std::fs::write(
        &path,
        br#"{"app_id":"io.other.App","state_path":"state","data_path":"data"}"#,
    )
    .unwrap();
    assert!(!libresync::companion::legacy_config_available(&path));
    std::fs::write(
        &path,
        br#"{"app_id":"com.codedbydan.libresync","state_path":"state","data_path":"data"}"#,
    )
    .unwrap();
    assert!(libresync::companion::legacy_config_available(&path));
}
#[test]
fn nearby_hints_refuse_other_apps_schema_digest_old_protocol_and_missing_metadata() {
    use libresync::companion::{discovery_compatibility, DiscoveryCompatibility};
    let manifest = momentum_manifest();
    let mut peer = libresync::DiscoveredPeer {
        identity: libresync::Identity::new("device", &manifest.app_id, "user"),
        addresses: vec![],
        advertisement: Some(libresync::PeerAdvertisement {
            display_name: "Phone".into(),
            device_kind: "phone".into(),
            role: "application".into(),
            app_display_name: "Momentum".into(),
            schema_version: 1,
            contract_digest: manifest.contract_digest().unwrap(),
        }),
        invitation: Some(libresync::PairingDescriptor {
            version: 2,
            invitation_id: "id".into(),
            inviter_fingerprint: "fingerprint".into(),
            expires_at: u64::MAX,
        }),
    };
    assert_eq!(
        discovery_compatibility(&manifest, &peer, 100).unwrap(),
        DiscoveryCompatibility::Ready
    );
    peer.identity.app_id = "other".into();
    assert_eq!(
        discovery_compatibility(&manifest, &peer, 100).unwrap(),
        DiscoveryCompatibility::DifferentApp
    );
    peer.identity.app_id = manifest.app_id.clone();
    peer.advertisement.as_mut().unwrap().schema_version = 2;
    assert_eq!(
        discovery_compatibility(&manifest, &peer, 100).unwrap(),
        DiscoveryCompatibility::UpgradeSchema
    );
    peer.advertisement.as_mut().unwrap().schema_version = 1;
    peer.advertisement.as_mut().unwrap().contract_digest = "wrong".into();
    assert_eq!(
        discovery_compatibility(&manifest, &peer, 100).unwrap(),
        DiscoveryCompatibility::UpgradeSchema
    );
    peer.advertisement.as_mut().unwrap().contract_digest = manifest.contract_digest().unwrap();
    peer.invitation.as_mut().unwrap().version = 1;
    assert_eq!(
        discovery_compatibility(&manifest, &peer, 100).unwrap(),
        DiscoveryCompatibility::UpgradePairing
    );
    peer.invitation.as_mut().unwrap().version = 2;
    peer.invitation.as_mut().unwrap().expires_at = 50;
    assert_eq!(
        discovery_compatibility(&manifest, &peer, 100).unwrap(),
        DiscoveryCompatibility::Expired
    );
    peer.advertisement = None;
    assert_eq!(
        discovery_compatibility(&manifest, &peer, 100).unwrap(),
        DiscoveryCompatibility::MissingMetadata
    );
}
#[test]
fn populated_companion_requires_consent_then_exports_premerge_without_rollback() {
    let root = tempfile::tempdir().unwrap();
    let app_dir = tempfile::tempdir().unwrap();
    let mut manager =
        CompanionManager::open(root.path(), "Mac", Arc::new(MemoryKeyStore::new()), false).unwrap();
    let id = manager
        .add_app(libresync::companion::notes_manifest())
        .unwrap();
    let companion = manager.session(&id).unwrap();
    companion
        .set("records", "local", b"local before merge".to_vec())
        .unwrap();
    let mut config = SessionConfig::new(
        app_dir.path(),
        DeviceMetadata {
            display_name: "Notes phone".into(),
            device_kind: "phone".into(),
            role: "application".into(),
            manifest: libresync::companion::notes_manifest(),
        },
    );
    config.advertise = false;
    let app = Session::open(config, Arc::new(MemoryKeyStore::new())).unwrap();
    app.set("records", "remote", b"remote note".to_vec())
        .unwrap();
    app.start().unwrap();
    companion
        .connect(&app.create_invitation(Duration::from_secs(120)).unwrap())
        .unwrap();
    let peer = app.snapshot().unwrap().identity.device_id;
    wait(|| companion.bootstrap_preview(&peer).unwrap().is_some());
    assert!(companion.get("records", "remote").unwrap().is_none());
    let preview = companion.bootstrap_preview(&peer).unwrap().unwrap();
    companion
        .resolve_bootstrap(&preview, libresync::BootstrapDecision::Merge)
        .unwrap();
    let recovery = companion.recovery_snapshots().unwrap();
    assert_eq!(recovery.len(), 1);
    assert_eq!(
        companion.recovery_snapshot_sizes().unwrap(),
        vec![(recovery[0].id.clone(), recovery[0].revision, 1)]
    );
    let exported = manager.export_recovery(&id, &recovery[0].id).unwrap();
    assert_eq!(exported.records.len(), 1);
    assert_eq!(companion.records("records").unwrap().len(), 2);
    assert!(manager.export_recovery(&id, "not-a-snapshot").is_err());
}
#[test]
fn explicit_legacy_recovery_copy_retention_is_bounded_without_pruning_originals() {
    let root = tempfile::tempdir().unwrap();
    let old = tempfile::tempdir().unwrap();
    let config = old.path().join("config.json");
    std::fs::write(&config, br#"{"app_id":"com.codedbydan.libresync"}"#).unwrap();
    let before = std::fs::read(&config).unwrap();
    let mut manager =
        CompanionManager::open(root.path(), "Mac", Arc::new(MemoryKeyStore::new()), false).unwrap();
    for _ in 0..7 {
        manager.archive_legacy(&config).unwrap();
    }
    assert_eq!(
        std::fs::read_dir(root.path().join("legacy-archives"))
            .unwrap()
            .count(),
        5
    );
    assert_eq!(std::fs::read(&config).unwrap(), before);
}
#[test]
fn failed_legacy_recovery_does_not_leave_partial_archive() {
    let root = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let config = source.path().join("config.json");
    std::fs::write(
        &config,
        serde_json::to_vec(&serde_json::json!({"data_path":source.path()})).unwrap(),
    )
    .unwrap();
    let mut manager =
        CompanionManager::open(root.path(), "Mac", Arc::new(MemoryKeyStore::new()), false).unwrap();
    assert!(manager.archive_legacy(&config).is_err());
    assert_eq!(
        std::fs::read_dir(root.path().join("legacy-archives"))
            .unwrap()
            .count(),
        0
    );
    assert!(config.exists());
}
