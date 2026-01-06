use crate::{AppKey, DeviceKeys, Error, Identity, Result};

pub trait DeviceHandler: Send + Sync {
    fn app_id(&self) -> &str;
    fn is_paired(&self, identity: &Identity) -> bool;
    fn approve_pair(&self, identity: &Identity) -> Result<bool>;
    fn app_key(&self) -> Result<AppKey> {
        Err(Error::Protocol("app key not configured".to_string()))
    }
    fn set_app_key(&self, _app_key: &AppKey) -> Result<()> {
        Err(Error::Protocol("app key updates not supported".to_string()))
    }
    fn device_keys(&self) -> Result<DeviceKeys> {
        Err(Error::Protocol("device keys not configured".to_string()))
    }
    fn is_paired_with_fingerprint(&self, identity: &Identity, fingerprint: &str) -> bool {
        let _ = fingerprint;
        self.is_paired(identity)
    }
    fn approve_pair_with_fingerprint(
        &self,
        identity: &Identity,
        fingerprint: &str,
    ) -> Result<bool> {
        let _ = fingerprint;
        self.approve_pair(identity)
    }
}

#[cfg(test)]
mod tests {
    use super::DeviceHandler;
    use crate::{AppKey, Error, Identity};

    struct StubHandler;

    impl DeviceHandler for StubHandler {
        fn app_id(&self) -> &str {
            "com.example.app"
        }

        fn is_paired(&self, _identity: &Identity) -> bool {
            false
        }

        fn approve_pair(&self, _identity: &Identity) -> crate::Result<bool> {
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
    fn default_pairing_methods_delegate() {
        let handler = StubHandler;
        let identity = Identity::new("device", "com.example.app", "user");
        assert!(!handler.is_paired_with_fingerprint(&identity, "fingerprint"));
        assert!(!handler
            .approve_pair_with_fingerprint(&identity, "fingerprint")
            .expect("approve"));
    }
}
