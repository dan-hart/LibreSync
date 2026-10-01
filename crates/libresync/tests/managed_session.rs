use libresync::{
    AdapterDescriptor, AppManifest, DeviceMetadata, MemoryKeyStore, Session, SessionConfig,
};
use std::sync::Arc;
fn config(path: &std::path::Path) -> SessionConfig {
    SessionConfig::new(
        path,
        DeviceMetadata {
            display_name: "Test device".into(),
            device_kind: "desktop".into(),
            role: "application".into(),
            manifest: AppManifest {
                app_id: "io.test.managed".into(),
                display_name: "Test".into(),
                schema_version: 1,
                adapters: vec![AdapterDescriptor {
                    id: "records".into(),
                    namespace: "io.test.managed".into(),
                    schema: "kv-v1".into(),
                    transactional: true,
                }],
            },
        },
    )
}
#[test]
fn durable_local_records_survive_restart_and_storage_is_encrypted() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(MemoryKeyStore::new());
    let session = Session::open(config(dir.path()), keys.clone()).unwrap();
    session
        .set("records", "hello", b"secret content".to_vec())
        .unwrap();
    let identity = session.snapshot().unwrap().identity;
    drop(session);
    let bytes = std::fs::read(dir.path().join("session.enc")).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("secret content"));
    let reopened = Session::open(config(dir.path()), keys).unwrap();
    assert_eq!(reopened.snapshot().unwrap().identity, identity);
    assert_eq!(
        reopened.get("records", "hello").unwrap(),
        Some(b"secret content".to_vec())
    );
}
#[test]
fn local_tombstones_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(MemoryKeyStore::new());
    let session = Session::open(config(dir.path()), keys.clone()).unwrap();
    session.set("records", "hello", b"value".to_vec()).unwrap();
    session.delete("records", "hello").unwrap();
    drop(session);
    let reopened = Session::open(config(dir.path()), keys).unwrap();
    assert_eq!(reopened.get("records", "hello").unwrap(), None);
    assert!(reopened.records("records").unwrap()[0].deleted);
}
fn wait_for(mut predicate: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    while !predicate() {
        assert!(
            std::time::Instant::now() < deadline,
            "automatic sync timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
#[test]
fn two_sessions_pair_and_automatically_sync_edits_resume_and_reopen() {
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let a_keys = Arc::new(MemoryKeyStore::new());
    let b_keys = Arc::new(MemoryKeyStore::new());
    let a = Session::open(config(a_dir.path()), a_keys).unwrap();
    let b = Session::open(config(b_dir.path()), b_keys.clone()).unwrap();
    a.start().unwrap();
    assert_eq!(a.start().unwrap(), a.address().unwrap());
    b.start().unwrap();
    a.set("records", "first", b"initial".to_vec()).unwrap();
    let mut invite = a
        .create_invitation(std::time::Duration::from_secs(30))
        .unwrap();
    invite.addresses = vec![std::net::SocketAddr::from((
        [127, 0, 0, 1],
        a.address().unwrap().port(),
    ))];
    let events = a.subscribe().unwrap();
    if let Err(e) = b.connect(&invite) {
        for event in events.try_iter() {
            eprintln!("{event:?}");
        }
        panic!("connect: {e}");
    }
    wait_for(|| b.get("records", "first").unwrap() == Some(b"initial".to_vec()));
    b.pause().unwrap();
    a.set("records", "first", b"edited".to_vec()).unwrap();
    b.resume().unwrap();
    wait_for(|| b.get("records", "first").unwrap() == Some(b"edited".to_vec()));
    b.shutdown().unwrap();
    drop(b);
    a.set("records", "first", b"restart".to_vec()).unwrap();
    let reopened = Session::open(config(b_dir.path()), b_keys).unwrap();
    reopened.start().unwrap();
    wait_for(|| reopened.get("records", "first").unwrap() == Some(b"restart".to_vec()));
    reopened.shutdown().unwrap();
    a.shutdown().unwrap();
}
#[test]
fn populated_bootstrap_requires_consent_and_rejects_stale_preview() {
    use libresync::BootstrapDecision;
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let a = Session::open(config(a_dir.path()), Arc::new(MemoryKeyStore::new())).unwrap();
    let b = Session::open(config(b_dir.path()), Arc::new(MemoryKeyStore::new())).unwrap();
    a.set("records", "a", b"a".to_vec()).unwrap();
    b.set("records", "b", b"b".to_vec()).unwrap();
    a.start().unwrap();
    b.start().unwrap();
    b.connect(
        &a.create_invitation(std::time::Duration::from_secs(30))
            .unwrap(),
    )
    .unwrap();
    let a_id = a.snapshot().unwrap().identity.device_id;
    let b_id = b.snapshot().unwrap().identity.device_id;
    wait_for(|| {
        b.bootstrap_preview(&a_id).unwrap().is_some()
            && a.bootstrap_preview(&b_id).unwrap().is_some()
    });
    assert_eq!(b.get("records", "a").unwrap(), None);
    assert_eq!(a.get("records", "b").unwrap(), None);
    let stale = b.bootstrap_preview(&a_id).unwrap().unwrap();
    b.set("records", "new", b"new".to_vec()).unwrap();
    assert!(b
        .resolve_bootstrap(&stale, BootstrapDecision::Merge)
        .is_err());
    wait_for(|| {
        b.bootstrap_preview(&a_id)
            .unwrap()
            .is_some_and(|p| p.local_revision != stale.local_revision)
    });
    b.resolve_bootstrap(
        &b.bootstrap_preview(&a_id).unwrap().unwrap(),
        BootstrapDecision::Merge,
    )
    .unwrap();
    a.resolve_bootstrap(
        &a.bootstrap_preview(&b_id).unwrap().unwrap(),
        BootstrapDecision::Merge,
    )
    .unwrap();
    wait_for(|| {
        a.get("records", "b").unwrap().is_some() && b.get("records", "a").unwrap().is_some()
    });
}
#[test]
fn remote_stored_is_distinct_from_app_applied_and_removal_preserves_data() {
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let a = Session::open(config(a_dir.path()), Arc::new(MemoryKeyStore::new())).unwrap();
    let b = Session::open(config(b_dir.path()), Arc::new(MemoryKeyStore::new())).unwrap();
    a.start().unwrap();
    b.start().unwrap();
    a.set("records", "a", b"a".to_vec()).unwrap();
    b.connect(
        &a.create_invitation(std::time::Duration::from_secs(30))
            .unwrap(),
    )
    .unwrap();
    let a_id = a.snapshot().unwrap().identity.device_id;
    let b_id = b.snapshot().unwrap().identity.device_id;
    wait_for(|| b.get("records", "a").unwrap().is_some());
    wait_for(|| a.snapshot().unwrap().peers[0].pending == 0);
    assert_eq!(a.snapshot().unwrap().peers[0].applied.sequence, 0);
    b.acknowledge_applied(&a_id, &b.stored_inbound_receipt(&a_id).unwrap())
        .unwrap();
    wait_for(|| a.snapshot().unwrap().peers[0].applied.sequence > 0);
    a.remove_peer(&b_id).unwrap();
    a.set("records", "later", b"later".to_vec()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(700));
    assert_eq!(b.get("records", "later").unwrap(), None);
    assert_eq!(a.get("records", "a").unwrap(), Some(b"a".to_vec()));
    assert!(b
        .connect(
            &a.create_invitation(std::time::Duration::from_secs(30))
                .unwrap()
        )
        .is_err());
}
#[test]
fn custom_managed_policy_rejects_immutable_payload_changes() {
    use libresync::{ManagedAdapter, ManagedRecord, PreparedChange, RecordsAdapter};
    struct Immutable(RecordsAdapter);
    impl ManagedAdapter for Immutable {
        fn descriptor(&self) -> AdapterDescriptor {
            self.0.descriptor()
        }
        fn prepare(
            &self,
            local: &[ManagedRecord],
            incoming: &[ManagedRecord],
            revision: u64,
        ) -> libresync::Result<PreparedChange> {
            for next in incoming {
                if let Some(old) = local.iter().find(|r| r.id == next.id) {
                    if !old.deleted && !next.deleted && old.value != next.value {
                        return Err(libresync::Error::Protocol(
                            "immutable payload collision".into(),
                        ));
                    }
                }
            }
            self.0.prepare(local, incoming, revision)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    let descriptor = cfg.metadata.manifest.adapters[0].clone();
    cfg = cfg
        .with_adapter(Arc::new(Immutable(RecordsAdapter::new(descriptor))))
        .unwrap();
    let s = Session::open(cfg, Arc::new(MemoryKeyStore::new())).unwrap();
    s.set("records", "op", b"first".to_vec()).unwrap();
    assert!(s.set("records", "op", b"different".to_vec()).is_err());
    assert_eq!(s.get("records", "op").unwrap(), Some(b"first".to_vec()));
    s.delete("records", "op").unwrap();
}
#[test]
fn duplicate_open_and_missing_storage_identity_fail_without_regeneration() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(MemoryKeyStore::new());
    let first = Session::open(config(dir.path()), keys.clone()).unwrap();
    first.set("records", "x", b"kept".to_vec()).unwrap();
    assert!(Session::open(config(dir.path()), keys).is_err());
    drop(first);
    assert!(Session::open(config(dir.path()), Arc::new(MemoryKeyStore::new())).is_err());
}
#[test]
fn failed_atomic_persistence_does_not_publish_a_local_edit() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(MemoryKeyStore::new());
    let session = Session::open(config(dir.path()), keys).unwrap();
    session.set("records", "x", b"before".to_vec()).unwrap();
    std::fs::rename(dir.path().join("session.enc"), dir.path().join("saved.enc")).unwrap();
    std::fs::create_dir(dir.path().join("session.enc")).unwrap();
    assert!(session.set("records", "x", b"after".to_vec()).is_err());
    assert_eq!(
        session.get("records", "x").unwrap(),
        Some(b"before".to_vec())
    );
    std::fs::remove_dir(dir.path().join("session.enc")).unwrap();
    std::fs::rename(dir.path().join("saved.enc"), dir.path().join("session.enc")).unwrap();
}
#[test]
fn old_application_inbox_acknowledgment_does_not_clear_newer_incoming_processing() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let a = Session::open(config(ad.path()), Arc::new(MemoryKeyStore::new())).unwrap();
    let b = Session::open(config(bd.path()), Arc::new(MemoryKeyStore::new())).unwrap();
    a.start().unwrap();
    b.start().unwrap();
    a.set("records", "x", b"first".to_vec()).unwrap();
    b.connect(
        &a.create_invitation(std::time::Duration::from_secs(30))
            .unwrap(),
    )
    .unwrap();
    wait_for(|| b.get("records", "x").unwrap().is_some());
    let old = b.application_inbox().unwrap();
    let a_id = a.snapshot().unwrap().identity.device_id;
    a.set("records", "x", b"second".to_vec()).unwrap();
    wait_for(|| b.get("records", "x").unwrap() == Some(b"second".to_vec()));
    b.acknowledge_inbox(&old).unwrap();
    let peer = b.snapshot().unwrap().peers.remove(0);
    assert!(peer.received.sequence > peer.processed.sequence);
    assert_eq!(peer.processed, old.receipts[&a_id]);
    let current = b.application_inbox().unwrap();
    b.acknowledge_inbox(&current).unwrap();
    b.acknowledge_inbox(&old).unwrap();
    let peer = b.snapshot().unwrap().peers.remove(0);
    assert_eq!(peer.received, peer.processed);
    assert_eq!(b.snapshot().unwrap().local_revision, 0);
}
#[test]
fn third_peer_forwarding_and_independent_apps_have_separate_enrollment() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let cd = tempfile::tempdir().unwrap();
    let xd = tempfile::tempdir().unwrap();
    let keys = Arc::new(MemoryKeyStore::new());
    let a = Session::open(config(ad.path()), keys.clone()).unwrap();
    let b = Session::open(config(bd.path()), keys.clone()).unwrap();
    let c = Session::open(config(cd.path()), keys.clone()).unwrap();
    let mut other = config(xd.path());
    other.metadata.manifest.app_id = "io.other.app".into();
    other.metadata.manifest.adapters[0].namespace = "io.other.app".into();
    let x = Session::open(SessionConfig::new(xd.path(), other.metadata), keys).unwrap();
    a.start().unwrap();
    b.start().unwrap();
    c.start().unwrap();
    x.start().unwrap();
    b.connect(
        &a.create_invitation(std::time::Duration::from_secs(30))
            .unwrap(),
    )
    .unwrap();
    c.connect(
        &a.create_invitation(std::time::Duration::from_secs(30))
            .unwrap(),
    )
    .unwrap();
    assert!(x
        .connect(
            &a.create_invitation(std::time::Duration::from_secs(30))
                .unwrap()
        )
        .is_err());
    b.set("records", "from-b", b"forward".to_vec()).unwrap();
    wait_for(|| c.get("records", "from-b").unwrap() == Some(b"forward".to_vec()));
    assert_eq!(x.get("records", "from-b").unwrap(), None);
    assert!(x.snapshot().unwrap().peers.is_empty());
    b.delete("records", "from-b").unwrap();
    wait_for(|| {
        c.records("records")
            .unwrap()
            .iter()
            .any(|r| r.id == "from-b" && r.deleted)
    });
}
#[test]
fn wake_syncs_promptly_even_with_long_periodic_catchup() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let mut ac = config(ad.path());
    ac.catch_up = std::time::Duration::from_secs(120);
    let mut bc = config(bd.path());
    bc.catch_up = std::time::Duration::from_secs(120);
    let a = Session::open(ac, Arc::new(MemoryKeyStore::new())).unwrap();
    let b = Session::open(bc, Arc::new(MemoryKeyStore::new())).unwrap();
    a.start().unwrap();
    b.start().unwrap();
    b.connect(
        &a.create_invitation(std::time::Duration::from_secs(30))
            .unwrap(),
    )
    .unwrap();
    a.set("records", "wake", b"wake".to_vec()).unwrap();
    wait_for(|| b.get("records", "wake").unwrap().is_some());
    let start = std::time::Instant::now();
    a.shutdown().unwrap();
    b.shutdown().unwrap();
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
}
#[test]
fn invitation_claim_matches_actual_inviter_identity() {
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let a = Session::open(config(ad.path()), Arc::new(MemoryKeyStore::new())).unwrap();
    let b = Session::open(config(bd.path()), Arc::new(MemoryKeyStore::new())).unwrap();
    a.start().unwrap();
    b.start().unwrap();
    let mut invitation = a
        .create_invitation(std::time::Duration::from_secs(30))
        .unwrap();
    invitation.identity.device_id = "forged-device".into();
    assert!(b.connect(&invitation).is_err());
    assert!(b.snapshot().unwrap().peers.is_empty());
}
#[test]
fn cancel_bootstrap_preserves_records_and_cursor_across_restart() {
    use libresync::BootstrapDecision;
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let bk = Arc::new(MemoryKeyStore::new());
    let a = Session::open(config(ad.path()), Arc::new(MemoryKeyStore::new())).unwrap();
    let b = Session::open(config(bd.path()), bk.clone()).unwrap();
    a.set("records", "a", b"a".to_vec()).unwrap();
    b.set("records", "b", b"b".to_vec()).unwrap();
    a.start().unwrap();
    b.start().unwrap();
    b.connect(
        &a.create_invitation(std::time::Duration::from_secs(30))
            .unwrap(),
    )
    .unwrap();
    let aid = a.snapshot().unwrap().identity.device_id;
    wait_for(|| b.bootstrap_preview(&aid).unwrap().is_some());
    let before = b.stored_inbound_receipt(&aid).unwrap();
    let preview = b.bootstrap_preview(&aid).unwrap().unwrap();
    b.resolve_bootstrap(&preview, BootstrapDecision::Cancel)
        .unwrap();
    assert_eq!(b.stored_inbound_receipt(&aid).unwrap(), before);
    b.shutdown().unwrap();
    drop(b);
    let reopened = Session::open(config(bd.path()), bk).unwrap();
    assert_eq!(reopened.get("records", "b").unwrap(), Some(b"b".to_vec()));
    assert_eq!(reopened.get("records", "a").unwrap(), None);
    assert_eq!(reopened.stored_inbound_receipt(&aid).unwrap(), before);
    assert!(reopened.bootstrap_preview(&aid).unwrap().is_none());
}
#[test]
fn session_code_entry_uses_advertised_descriptor_and_rechecks_contract() {
    use libresync::{DiscoveredPeer, PeerAdvertisement};
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let a = Session::open(config(ad.path()), Arc::new(MemoryKeyStore::new())).unwrap();
    let b = Session::open(config(bd.path()), Arc::new(MemoryKeyStore::new())).unwrap();
    a.start().unwrap();
    b.start().unwrap();
    let invite = a
        .create_code_invitation(std::time::Duration::from_secs(30))
        .unwrap();
    let metadata = &invite.invitation.metadata;
    let mut peer = DiscoveredPeer {
        identity: invite.identity.clone(),
        addresses: invite.addresses.clone(),
        invitation: Some(invite.invitation.descriptor()),
        advertisement: Some(PeerAdvertisement {
            display_name: metadata.display_name.clone(),
            device_kind: metadata.device_kind.clone(),
            role: metadata.role.clone(),
            app_display_name: metadata.manifest.display_name.clone(),
            schema_version: metadata.manifest.schema_version,
            contract_digest: metadata.manifest.contract_digest().unwrap(),
        }),
    };
    let wrong = if invite.invitation.secret == "000000" {
        "000001"
    } else {
        "000000"
    };
    assert!(b.connect_code(&peer, wrong).is_err());
    b.connect_code(&peer, &invite.invitation.secret).unwrap();
    peer.advertisement.as_mut().unwrap().contract_digest = "different".into();
    assert!(b.connect_code(&peer, &invite.invitation.secret).is_err());
}
#[test]
fn existing_enrolled_groups_refuse_implicit_transport_key_replacement() {
    let dirs: Vec<_> = (0..4).map(|_| tempfile::tempdir().unwrap()).collect();
    let sessions: Vec<_> = dirs
        .iter()
        .map(|d| Session::open(config(d.path()), Arc::new(MemoryKeyStore::new())).unwrap())
        .collect();
    for s in &sessions {
        s.start().unwrap();
    }
    sessions[1]
        .connect(
            &sessions[0]
                .create_invitation(std::time::Duration::from_secs(30))
                .unwrap(),
        )
        .unwrap();
    sessions[3]
        .connect(
            &sessions[2]
                .create_invitation(std::time::Duration::from_secs(30))
                .unwrap(),
        )
        .unwrap();
    assert!(sessions[1]
        .connect(
            &sessions[2]
                .create_invitation(std::time::Duration::from_secs(30))
                .unwrap()
        )
        .is_err());
    sessions[0]
        .set("records", "stable", b"stable".to_vec())
        .unwrap();
    wait_for(|| sessions[1].get("records", "stable").unwrap().is_some());
}
#[test]
fn populated_merge_retains_encrypted_premerge_recovery_snapshot() {
    use libresync::BootstrapDecision;
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let a = Session::open(config(ad.path()), Arc::new(MemoryKeyStore::new())).unwrap();
    let b = Session::open(config(bd.path()), Arc::new(MemoryKeyStore::new())).unwrap();
    a.set("records", "a", b"remote".to_vec()).unwrap();
    b.set("records", "b", b"before-merge".to_vec()).unwrap();
    a.start().unwrap();
    b.start().unwrap();
    b.connect(
        &a.create_invitation(std::time::Duration::from_secs(30))
            .unwrap(),
    )
    .unwrap();
    let aid = a.snapshot().unwrap().identity.device_id;
    wait_for(|| b.bootstrap_preview(&aid).unwrap().is_some());
    b.resolve_bootstrap(
        &b.bootstrap_preview(&aid).unwrap().unwrap(),
        BootstrapDecision::Merge,
    )
    .unwrap();
    let backups = b.recovery_snapshots().unwrap();
    assert_eq!(backups.len(), 1);
    assert!(backups[0]
        .records
        .iter()
        .any(|r| r.id == "b" && r.value == b"before-merge"));
    assert!(
        !String::from_utf8_lossy(&std::fs::read(bd.path().join("session.enc")).unwrap())
            .contains("before-merge")
    );
}
#[test]
fn partial_identity_bundle_and_unavailable_secure_storage_never_regenerate_identity() {
    use libresync::KeyStore;
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(MemoryKeyStore::new());
    let session = Session::open(config(dir.path()), keys.clone()).unwrap();
    drop(session);
    let namespace = Sha256::digest(
        std::fs::canonicalize(dir.path())
            .unwrap()
            .to_string_lossy()
            .as_bytes(),
    )
    .iter()
    .map(|b| format!("{b:02x}"))
    .collect::<String>();
    let name = format!("managed-{namespace}-identity-v1");
    let mut bundle: serde_json::Value =
        serde_json::from_slice(&keys.get(&name).unwrap().unwrap()).unwrap();
    bundle["private_key"] = serde_json::json!([]);
    let corrupt = serde_json::to_vec(&bundle).unwrap();
    keys.set(&name, &corrupt).unwrap();
    assert!(Session::open(config(dir.path()), keys.clone()).is_err());
    assert_eq!(keys.get(&name).unwrap().unwrap(), corrupt);
    struct Unavailable(std::sync::atomic::AtomicUsize);
    impl KeyStore for Unavailable {
        fn get(&self, _: &str) -> libresync::Result<Option<Vec<u8>>> {
            Err(
                std::io::Error::new(std::io::ErrorKind::PermissionDenied, "secure store locked")
                    .into(),
            )
        }
        fn set(&self, _: &str, _: &[u8]) -> libresync::Result<()> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
        fn delete(&self, _: &str) -> libresync::Result<()> {
            Ok(())
        }
    }
    let fresh = tempfile::tempdir().unwrap();
    let unavailable = Arc::new(Unavailable(std::sync::atomic::AtomicUsize::new(0)));
    assert!(Session::open(config(fresh.path()), unavailable.clone()).is_err());
    assert_eq!(unavailable.0.load(std::sync::atomic::Ordering::SeqCst), 0);
}
#[test]
fn platform_unknown_is_preserved_and_slow_event_consumers_do_not_block_local_commits() {
    use libresync::{DiagnosticAction, DiagnosticEvidence, PermissionState, SessionEvent};
    let dir = tempfile::tempdir().unwrap();
    let session = Session::open(config(dir.path()), Arc::new(MemoryKeyStore::new())).unwrap();
    let _slow = session.subscribe().unwrap();
    for _ in 0..300 {
        session.report_platform_evidence(
            "ios".into(),
            "local-network".into(),
            PermissionState::Unknown,
        );
    }
    session
        .set("records", "responsive", b"yes".to_vec())
        .unwrap();
    assert!(matches!(
        session.snapshot().unwrap().diagnostics.last().unwrap(),
        SessionEvent::Diagnostic {
            action: DiagnosticAction::CheckPermissions,
            evidence: DiagnosticEvidence::Platform {
                state: PermissionState::Unknown,
                ..
            },
            ..
        }
    ));
}
#[test]
fn newer_remote_bootstrap_candidate_invalidates_consent_without_advancing_cursor() {
    use libresync::{BootstrapDecision, Error, SessionErrorCode};
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let a = Session::open(config(ad.path()), Arc::new(MemoryKeyStore::new())).unwrap();
    let b = Session::open(config(bd.path()), Arc::new(MemoryKeyStore::new())).unwrap();
    a.set("records", "remote", b"first".to_vec()).unwrap();
    b.set("records", "local", b"local".to_vec()).unwrap();
    a.start().unwrap();
    b.start().unwrap();
    b.connect(
        &a.create_invitation(std::time::Duration::from_secs(30))
            .unwrap(),
    )
    .unwrap();
    let aid = a.snapshot().unwrap().identity.device_id;
    wait_for(|| b.bootstrap_preview(&aid).unwrap().is_some());
    let old = b.bootstrap_preview(&aid).unwrap().unwrap();
    let cursor = b.stored_inbound_receipt(&aid).unwrap();
    a.set("records", "remote", b"newer".to_vec()).unwrap();
    wait_for(|| {
        b.bootstrap_preview(&aid)
            .unwrap()
            .is_some_and(|p| p.batch != old.batch)
    });
    assert!(matches!(
        b.resolve_bootstrap(&old, BootstrapDecision::Merge),
        Err(Error::Managed {
            code: SessionErrorCode::StaleBootstrap,
            ..
        })
    ));
    assert_eq!(b.stored_inbound_receipt(&aid).unwrap(), cursor);
    assert_eq!(b.get("records", "remote").unwrap(), None);
    b.resolve_bootstrap(
        &b.bootstrap_preview(&aid).unwrap().unwrap(),
        BootstrapDecision::Merge,
    )
    .unwrap();
    assert_eq!(b.get("records", "remote").unwrap(), Some(b"newer".to_vec()));
}
#[test]
fn local_edits_while_paused_remain_pending_and_resume_automatically() {
    use libresync::PeerState;
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let a = Session::open(config(ad.path()), Arc::new(MemoryKeyStore::new())).unwrap();
    let b = Session::open(config(bd.path()), Arc::new(MemoryKeyStore::new())).unwrap();
    a.start().unwrap();
    b.start().unwrap();
    b.connect(
        &a.create_invitation(std::time::Duration::from_secs(30))
            .unwrap(),
    )
    .unwrap();
    a.set("records", "x", b"first".to_vec()).unwrap();
    wait_for(|| b.get("records", "x").unwrap().is_some());
    let inbox = b.application_inbox().unwrap();
    b.acknowledge_inbox(&inbox).unwrap();
    wait_for(|| a.snapshot().unwrap().peers[0].applied.sequence > 0);
    a.pause().unwrap();
    a.set("records", "x", b"queued".to_vec()).unwrap();
    let status = a.snapshot().unwrap();
    assert_eq!(status.peers[0].state, PeerState::Paused);
    assert_eq!(status.peers[0].pending, 1);
    assert_eq!(b.get("records", "x").unwrap(), Some(b"first".to_vec()));
    a.resume().unwrap();
    wait_for(|| b.get("records", "x").unwrap() == Some(b"queued".to_vec()));
}

#[test]
fn independent_processes_discover_pair_and_deliver_applied_receipts() {
    use std::{process::Command, time::Duration};
    const ROLE: &str = "LIBRESYNC_PROCESS_TEST_ROLE";
    const DIR: &str = "LIBRESYNC_PROCESS_TEST_DIR";
    if let Ok(role) = std::env::var(ROLE) {
        let root = std::path::PathBuf::from(std::env::var(DIR).unwrap());
        let mut c = config(&root.join(&role));
        c.metadata.manifest.app_id = format!(
            "test.process.{}",
            root.file_name().unwrap().to_string_lossy()
        );
        c.metadata.manifest.adapters[0].namespace = c.metadata.manifest.app_id.clone();
        let c = SessionConfig::new(root.join(&role), c.metadata);
        let s = Session::open(c, Arc::new(MemoryKeyStore::new())).unwrap();
        s.start().unwrap();
        if role == "server" {
            s.set("records", "process", b"process-value".to_vec())
                .unwrap();
            let invitation = s.create_code_invitation(Duration::from_secs(30)).unwrap();
            std::fs::write(root.join("invitation.tmp"), invitation.encode().unwrap()).unwrap();
            std::fs::rename(root.join("invitation.tmp"), root.join("invitation")).unwrap();
            wait_for(|| {
                s.snapshot()
                    .unwrap()
                    .peers
                    .iter()
                    .any(|p| p.applied.sequence > 0 && p.pending == 0)
            });
        } else {
            wait_for(|| root.join("invitation").exists());
            let invitation = libresync::SessionInvitation::decode(
                &std::fs::read_to_string(root.join("invitation")).unwrap(),
            )
            .unwrap();
            wait_for(|| {
                s.discovered_peers()
                    .unwrap()
                    .iter()
                    .any(|p| p.identity == invitation.identity && p.invitation.is_some())
            });
            let peer = s
                .discovered_peers()
                .unwrap()
                .into_iter()
                .find(|p| p.identity == invitation.identity && p.invitation.is_some())
                .unwrap();
            s.connect_code(&peer, &invitation.invitation.secret)
                .unwrap();
            wait_for(|| s.get("records", "process").unwrap() == Some(b"process-value".to_vec()));
            let inbox = s.application_inbox().unwrap();
            let app_file = root.join("app-committed.json");
            let mut file = std::fs::File::create(app_file).unwrap();
            use std::io::Write;
            file.write_all(&serde_json::to_vec(&inbox.records).unwrap())
                .unwrap();
            file.sync_all().unwrap();
            s.acknowledge_inbox(&inbox).unwrap();
            wait_for(|| {
                s.snapshot()
                    .unwrap()
                    .peers
                    .iter()
                    .any(|p| p.processed.sequence > 0)
            });
            // Keep the authenticated listener alive until the sender observes Applied.
            wait_for(|| root.join("server-finished").exists());
        }
        if role == "server" {
            std::fs::write(root.join("server-finished"), b"done").unwrap();
        }
        s.shutdown().unwrap();
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let log = std::fs::File::create(dir.path().join("workers.log")).unwrap();
    let spawn = |role: &str| {
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "independent_processes_discover_pair_and_deliver_applied_receipts",
                "--nocapture",
            ])
            .env(ROLE, role)
            .env(DIR, dir.path())
            .stdout(log.try_clone().unwrap())
            .stderr(log.try_clone().unwrap())
            .spawn()
            .unwrap()
    };
    let mut server = spawn("server");
    let mut client = spawn("client");
    let client_status = client.wait().unwrap();
    let server_status = server.wait().unwrap();
    let output = std::fs::read_to_string(dir.path().join("workers.log")).unwrap();
    assert!(
        server_status.success() && client_status.success(),
        "process test: {output}"
    );
}

#[test]
fn stopping_one_discovery_runtime_preserves_other_app_registrations() {
    let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
    let a = Session::open(config(dirs[0].path()), Arc::new(MemoryKeyStore::new())).unwrap();
    let b = Session::open(config(dirs[1].path()), Arc::new(MemoryKeyStore::new())).unwrap();
    a.start().unwrap();
    b.start().unwrap();
    let aid = a.snapshot().unwrap().identity;
    wait_for(|| {
        b.discovered_peers()
            .unwrap()
            .iter()
            .any(|p| p.identity == aid)
    });
    a.shutdown().unwrap();
    drop(a);
    let c = Session::open(config(dirs[2].path()), Arc::new(MemoryKeyStore::new())).unwrap();
    c.start().unwrap();
    let invitation = b
        .create_code_invitation(std::time::Duration::from_secs(30))
        .unwrap();
    wait_for(|| {
        c.discovered_peers()
            .unwrap()
            .iter()
            .any(|p| p.identity == invitation.identity && p.invitation.is_some())
    });
    let peer = c
        .discovered_peers()
        .unwrap()
        .into_iter()
        .find(|p| p.identity == invitation.identity && p.invitation.is_some())
        .unwrap();
    c.connect_code(&peer, &invitation.invitation.secret)
        .unwrap();
    b.set("records", "after-stop", b"still-running".to_vec())
        .unwrap();
    wait_for(|| c.get("records", "after-stop").unwrap() == Some(b"still-running".to_vec()));
}
