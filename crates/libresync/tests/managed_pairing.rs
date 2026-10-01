use libresync::{AdapterDescriptor, AppManifest, DeviceMetadata, PairingManager};
use std::time::Duration;
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
#[test]
fn invitation_roundtrip_and_revocation() {
    let manager = PairingManager::new();
    let invite = manager
        .create_invitation(metadata(), "00".repeat(32), Duration::from_secs(300))
        .unwrap();
    let encoded = invite.encode().unwrap();
    assert_eq!(
        libresync::PairingInvitation::decode(&encoded).unwrap(),
        invite
    );
    assert_eq!(invite.secret.len(), 64);
    assert!(manager.is_open(&invite.invitation_id));
    manager.revoke(&invite.invitation_id).unwrap();
    assert!(!manager.is_open(&invite.invitation_id));
}
#[test]
fn manifests_require_same_adapter_contract() {
    let a = metadata().manifest;
    let mut b = a.clone();
    b.adapters[0].schema = "incompatible".into();
    assert!(!a.compatible_with(&b));
}
use libresync::{
    link_secure, AppKey, DeviceHandler, DeviceKeys, Identity, SecurePairingOutcome, State,
    SyncListener, SyncOptions,
};
use std::sync::{Arc, Mutex};
struct Handler {
    keys: DeviceKeys,
    manager: Arc<PairingManager>,
    key: Mutex<AppKey>,
    links: Mutex<Vec<SecurePairingOutcome>>,
}
impl DeviceHandler for Handler {
    fn app_id(&self) -> &str {
        "test.app"
    }
    fn is_linked(&self, identity: &Identity) -> bool {
        self.links
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.identity == *identity)
    }
    fn approve_link(&self, _: &Identity) -> libresync::Result<bool> {
        Ok(false)
    }
    fn app_key(&self) -> libresync::Result<AppKey> {
        Ok(self.key.lock().unwrap().clone())
    }
    fn device_keys(&self) -> libresync::Result<DeviceKeys> {
        Ok(self.keys.clone())
    }
    fn allow_legacy_link(&self) -> bool {
        false
    }
    fn device_metadata(&self) -> Option<DeviceMetadata> {
        Some(metadata())
    }
    fn pairing_manager(&self) -> Option<Arc<PairingManager>> {
        Some(self.manager.clone())
    }
    fn prepare_secure_pairing(&self, _: &SecurePairingOutcome) -> libresync::Result<()> {
        Ok(())
    }
    fn commit_secure_pairing(
        &self,
        id: &str,
        identity: &Identity,
        metadata: &DeviceMetadata,
        fp: &str,
        key: &AppKey,
    ) -> libresync::Result<()> {
        let mut links = self.links.lock().unwrap();
        let mut current = self.key.lock().unwrap();
        if !links.is_empty() && *current != *key {
            return Err(libresync::Error::Protocol(
                "established group cannot rekey".into(),
            ));
        }
        *current = key.clone();
        links.push(SecurePairingOutcome {
            invitation_id: id.into(),
            identity: identity.clone(),
            metadata: metadata.clone(),
            fingerprint: fp.into(),
        });
        Ok(())
    }
    fn recover_secure_pairing(
        &self,
        id: &str,
        identity: &Identity,
        fp: &str,
    ) -> libresync::Result<Option<DeviceMetadata>> {
        Ok(self
            .links
            .lock()
            .unwrap()
            .iter()
            .find(|l| l.invitation_id == id && l.identity == *identity && l.fingerprint == fp)
            .map(|l| l.metadata.clone()))
    }
}
fn handler(id: &str) -> Arc<Handler> {
    let identity = Identity::new(id, "test.app", "u");
    Arc::new(Handler {
        keys: DeviceKeys::generate(&identity).unwrap(),
        manager: Arc::new(PairingManager::new()),
        key: Mutex::new(AppKey::generate().unwrap()),
        links: Mutex::new(Vec::new()),
    })
}
fn listener(h: Arc<Handler>) -> SyncListener {
    SyncListener::start(
        "127.0.0.1:0".parse().unwrap(),
        Identity::new("server", "test.app", "u"),
        Arc::new(Mutex::new(State::new("server"))),
        h,
    )
    .unwrap()
}
fn pair(
    server: &SyncListener,
    client: &Handler,
    invite: &libresync::PairingInvitation,
) -> libresync::Result<SecurePairingOutcome> {
    link_secure(
        &Identity::new("client", "test.app", "u"),
        &metadata(),
        server.addr(),
        &client.keys,
        invite,
        client,
        &SyncOptions::default(),
    )
}
#[test]
fn correct_code_authenticates_and_inviter_key_is_authoritative() {
    let server = handler("server");
    let client = handler("client");
    let l = listener(server.clone());
    let invite = server
        .manager
        .create_code_invitation(metadata(), server.keys.fingerprint().into())
        .unwrap();
    let original = server.app_key().unwrap();
    let outcome = pair(&l, &client, &invite).unwrap();
    assert_eq!(outcome.identity.device_id, "server");
    assert_eq!(client.app_key().unwrap(), original);
    assert_eq!(server.app_key().unwrap(), original);
    assert!(server.is_linked(&Identity::new("client", "test.app", "u")));
    assert!(pair(&l, &handler("client"), &invite).is_err()); // Replay from a different TLS identity.
    l.shutdown().unwrap();
}
#[test]
fn wrong_code_never_commits_or_discloses_key_and_attempts_are_bounded() {
    let server = handler("server");
    let client = handler("client");
    let l = listener(server.clone());
    let mut invite = server
        .manager
        .create_code_invitation(metadata(), server.keys.fingerprint().into())
        .unwrap();
    let correct = invite.clone();
    invite.secret = if invite.secret == "000000" {
        "111111".into()
    } else {
        "000000".into()
    };
    for _ in 0..5 {
        assert!(pair(&l, &client, &invite).is_err());
    }
    assert!(server.links.lock().unwrap().is_empty());
    assert!(client.links.lock().unwrap().is_empty());
    assert!(pair(&l, &client, &correct).is_err());
    l.shutdown().unwrap();
}
#[test]
fn expired_revoked_closed_and_wrong_certificate_are_rejected() {
    let server = handler("server");
    let client = handler("client");
    let l = listener(server.clone());
    let mut invite = server
        .manager
        .create_invitation(
            metadata(),
            server.keys.fingerprint().into(),
            Duration::from_secs(300),
        )
        .unwrap();
    invite.expires_at = 0;
    assert!(pair(&l, &client, &invite).is_err());
    let invite = server
        .manager
        .create_invitation(
            metadata(),
            server.keys.fingerprint().into(),
            Duration::from_secs(300),
        )
        .unwrap();
    let mut wrong = invite.clone();
    wrong.inviter_fingerprint = "00".repeat(32);
    assert!(pair(&l, &client, &wrong).is_err());
    server.manager.revoke(&invite.invitation_id).unwrap();
    assert!(pair(&l, &client, &invite).is_err());
    server.manager.close().unwrap();
    assert!(pair(&l, &client, &invite).is_err());
    l.shutdown().unwrap();
}
#[test]
fn legacy_link_is_rejected_by_managed_handler() {
    let server = handler("server");
    let client = handler("client");
    let l = listener(server.clone());
    assert!(libresync::link_with_device(
        &Identity::new("client", "test.app", "u"),
        &client.keys,
        &client.app_key().unwrap(),
        l.addr(),
        None
    )
    .is_err());
    assert!(server.links.lock().unwrap().is_empty());
    l.shutdown().unwrap();
}
#[test]
fn incompatible_manifest_and_existing_group_rekey_are_refused() {
    let server = handler("server");
    let client = handler("client");
    let l = listener(server.clone());
    let invite = server
        .manager
        .create_code_invitation(metadata(), server.keys.fingerprint().into())
        .unwrap();
    let mut m = metadata();
    m.manifest.schema_version = 2;
    assert!(link_secure(
        &Identity::new("client", "test.app", "u"),
        &m,
        l.addr(),
        &client.keys,
        &invite,
        client.as_ref(),
        &SyncOptions::default()
    )
    .is_err());
    client.links.lock().unwrap().push(SecurePairingOutcome {
        invitation_id: "other".into(),
        identity: Identity::new("old", "test.app", "u"),
        metadata: metadata(),
        fingerprint: "00".repeat(32),
    });
    assert!(pair(&l, &client, &invite).is_err());
    l.shutdown().unwrap();
}
#[test]
fn discovery_contract_digest_ignores_friendly_names_and_adapter_order() {
    let a = metadata().manifest;
    let mut b = a.clone();
    b.display_name = "Different label".into();
    assert_eq!(a.contract_digest().unwrap(), b.contract_digest().unwrap());
    b.schema_version = 2;
    assert_ne!(a.contract_digest().unwrap(), b.contract_digest().unwrap());
}
#[test]
fn managed_listener_refuses_legacy_sync_even_for_enrolled_peer() {
    let server = handler("server");
    let client = handler("client");
    let l = listener(server.clone());
    let invitation = server
        .manager
        .create_code_invitation(metadata(), server.keys.fingerprint().into())
        .unwrap();
    pair(&l, &client, &invitation).unwrap();
    assert!(libresync::sync_with_device(
        &Identity::new("client", "test.app", "u"),
        &mut State::new("client"),
        l.addr(),
        &client.keys,
        &client.app_key().unwrap(),
        |_, _| Ok(())
    )
    .is_err());
    l.shutdown().unwrap();
}
#[test]
fn public_code_descriptor_contains_no_secret_and_constructs_credential() {
    let manager = PairingManager::new();
    let invite = manager
        .create_code_invitation(metadata(), "00".repeat(32))
        .unwrap();
    let descriptor = invite.descriptor();
    let public = serde_json::to_string(&descriptor).unwrap();
    assert!(!public.contains(&invite.secret));
    assert_eq!(
        descriptor
            .with_code(invite.metadata.clone(), &invite.secret)
            .unwrap(),
        invite
    );
}
