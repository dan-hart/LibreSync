use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct Identity {
    pub device_id: String,
    pub app_id: String,
    pub user_id: String,
}

impl Identity {
    pub fn new(device_id: impl Into<String>, app_id: impl Into<String>, user_id: impl Into<String>) -> Self {
        Self {
            device_id: device_id.into(),
            app_id: app_id.into(),
            user_id: user_id.into(),
        }
    }

    pub fn matches_app(&self, app_id: &str) -> bool {
        self.app_id == app_id
    }
}

#[cfg(test)]
mod tests {
    use super::Identity;

    #[test]
    fn identity_matches_app() {
        let identity = Identity::new("device", "com.example.app", "user");
        assert!(identity.matches_app("com.example.app"));
        assert!(!identity.matches_app("com.example.other"));
    }
}
