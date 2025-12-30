use crate::{Identity, Result};

pub trait DeviceHandler: Send + Sync {
    fn app_id(&self) -> &str;
    fn is_paired(&self, identity: &Identity) -> bool;
    fn approve_pair(&self, identity: &Identity) -> Result<bool>;
}
