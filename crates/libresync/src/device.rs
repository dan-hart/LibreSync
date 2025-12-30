use crate::{DeviceKeys, Error, Identity, Result};

pub trait DeviceHandler: Send + Sync {
    fn app_id(&self) -> &str;
    fn is_paired(&self, identity: &Identity) -> bool;
    fn approve_pair(&self, identity: &Identity) -> Result<bool>;
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
