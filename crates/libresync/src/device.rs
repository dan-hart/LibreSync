use crate::{AppKey, DeviceKeys, Error, Identity, Result};

pub trait DeviceHandler: Send + Sync {
    /// Low-level Engine compatibility only. Managed handlers MUST return false.
    fn allow_legacy_link(&self) -> bool {
        true
    }
    fn device_metadata(&self) -> Option<crate::DeviceMetadata> {
        None
    }
    fn pairing_manager(&self) -> Option<std::sync::Arc<crate::PairingManager>> {
        None
    }
    /// Idle I/O budget after authenticated PAKE readiness; initial proof remains short.
    fn authenticated_pairing_io_timeout(&self) -> Option<std::time::Duration> {
        None
    }
    /// Persist an authenticated server challenge before sending durable preparation proof.
    /// This journal permits recovery if the server commits but acceptance is
    /// lost. Do not activate trust or replace the group key at this step.
    fn prepare_secure_pairing(&self, _pending: &crate::SecurePairingOutcome) -> Result<()> {
        Err(Error::Protocol(
            "durable pairing preparation not configured".into(),
        ))
    }
    /// Atomically and durably save enrollment journal, trust and group key before
    /// returning. Refuse group-key replacement when any other peer is enrolled.
    /// Must check cancellation/revocation under the same enrollment lock.
    fn commit_secure_pairing(
        &self,
        _invitation_id: &str,
        _identity: &Identity,
        _metadata: &crate::DeviceMetadata,
        _fingerprint: &str,
        _key: &AppKey,
    ) -> Result<()> {
        Err(Error::Protocol(
            "durable managed pairing not configured".into(),
        ))
    }
    /// Only return a journaled enrollment for this exact invitation, identity
    /// AND observed certificate fingerprint. Must recheck revocation.
    fn recover_secure_pairing(
        &self,
        _invitation_id: &str,
        _identity: &Identity,
        _fingerprint: &str,
    ) -> Result<Option<crate::DeviceMetadata>> {
        Ok(None)
    }

    fn app_id(&self) -> &str;
    fn is_linked(&self, identity: &Identity) -> bool;
    fn approve_link(&self, identity: &Identity) -> Result<bool>;
    fn app_key(&self) -> Result<AppKey> {
        Err(Error::Protocol("app key not configured".to_string()))
    }
    fn set_app_key(&self, _app_key: &AppKey) -> Result<()> {
        Err(Error::Protocol("app key updates not supported".to_string()))
    }
    fn device_keys(&self) -> Result<DeviceKeys> {
        Err(Error::Protocol("device keys not configured".to_string()))
    }
    fn set_device_keys(&self, _device_keys: &DeviceKeys) -> Result<()> {
        Err(Error::Protocol(
            "device key updates not supported".to_string(),
        ))
    }
    fn is_linked_with_fingerprint(&self, identity: &Identity, fingerprint: &str) -> bool {
        let _ = fingerprint;
        self.is_linked(identity)
    }
    fn approve_link_with_fingerprint(
        &self,
        identity: &Identity,
        fingerprint: &str,
    ) -> Result<bool> {
        let _ = fingerprint;
        self.approve_link(identity)
    }
    fn pairing_secret(&self) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::DeviceHandler;
    use crate::{AppKey, DeviceKeys, Error, Identity};

    struct StubHandler;

    impl DeviceHandler for StubHandler {
        fn app_id(&self) -> &str {
            "com.example.app"
        }

        fn is_linked(&self, _identity: &Identity) -> bool {
            false
        }

        fn approve_link(&self, _identity: &Identity) -> crate::Result<bool> {
            Ok(false)
        }
    }

    #[test]
    fn default_app_key_is_missing() {
        let handler = StubHandler;
        let error = handler.app_key().expect_err("missing app key");
        assert!(matches!(error, Error::Protocol(_)));
    }

    #[test]
    fn default_device_keys_is_missing() {
        let handler = StubHandler;
        let error = handler.device_keys().expect_err("missing device keys");
        assert!(matches!(error, Error::Protocol(_)));
    }

    #[test]
    fn default_set_app_key_is_unsupported() {
        let handler = StubHandler;
        let app_key = AppKey::generate().expect("app key");
        let error = handler.set_app_key(&app_key).expect_err("no set app key");
        assert!(matches!(error, Error::Protocol(_)));
    }

    #[test]
    fn default_set_device_keys_is_unsupported() {
        let handler = StubHandler;
        let identity = Identity::new("device", "com.example.app", "user");
        let device_keys = DeviceKeys::generate(&identity).expect("device keys");
        let error = handler
            .set_device_keys(&device_keys)
            .expect_err("no set device keys");
        assert!(matches!(error, Error::Protocol(_)));
    }

    #[test]
    fn default_linking_methods_delegate() {
        let handler = StubHandler;
        let identity = Identity::new("device", "com.example.app", "user");
        assert!(!handler.is_linked_with_fingerprint(&identity, "fingerprint"));
        assert!(!handler
            .approve_link_with_fingerprint(&identity, "fingerprint")
            .expect("approve"));
    }
}
