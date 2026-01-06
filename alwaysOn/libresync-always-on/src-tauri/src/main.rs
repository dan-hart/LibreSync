#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use libresync::{AppKey, DeviceKeys, Engine, EngineConfig, Identity, State};
use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use uuid::Uuid;

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let paths = app_paths(app).map_err(|error| error.to_string())?;
            let (mut config, keys, app_key) =
                load_or_init_config(&paths.config).map_err(|error| error.to_string())?;
            ensure_backup_policy(&mut config, &paths.backups)
                .map_err(|error| error.to_string())?;
            save_config(&paths.config, &config).map_err(|error| error.to_string())?;

            // TODO: initialize the always-on Engine and adapters here.
            let identity = config.identity();
            let handler = AlwaysOnHandler {
                app_id: identity.app_id.clone(),
                keys,
                app_key,
            };
            let _engine = Engine::new(
                EngineConfig::new(identity),
                State::new(config.device_id.clone()),
                std::sync::Arc::new(handler),
            );
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running LibreSyncAlwaysOn");
}

struct AlwaysOnHandler {
    app_id: String,
    keys: DeviceKeys,
    app_key: AppKey,
}

impl libresync::DeviceHandler for AlwaysOnHandler {
    fn app_id(&self) -> &str {
        &self.app_id
    }

    fn is_paired(&self, _identity: &libresync::Identity) -> bool {
        false
    }

    fn approve_pair(&self, _identity: &libresync::Identity) -> libresync::Result<bool> {
        Ok(false)
    }

    fn device_keys(&self) -> libresync::Result<DeviceKeys> {
        Ok(self.keys.clone())
    }

    fn app_key(&self) -> libresync::Result<AppKey> {
        Ok(self.app_key.clone())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceKeysRecord {
    cert_der: String,
    key_der: String,
    fingerprint: String,
}

impl DeviceKeysRecord {
    fn from_keys(keys: &DeviceKeys) -> Self {
        Self {
            cert_der: BASE64.encode(keys.cert_der()),
            key_der: BASE64.encode(keys.key_der()),
            fingerprint: keys.fingerprint().to_string(),
        }
    }

    fn to_keys(&self) -> Result<DeviceKeys, String> {
        let cert_der = BASE64
            .decode(self.cert_der.as_bytes())
            .map_err(|error| error.to_string())?;
        let key_der = BASE64
            .decode(self.key_der.as_bytes())
            .map_err(|error| error.to_string())?;
        DeviceKeys::from_der(cert_der, key_der).map_err(|error| error.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct BackupPolicy {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    allow_restore: bool,
    #[serde(default)]
    backup_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AlwaysOnConfig {
    app_id: String,
    device_id: String,
    user_id: String,
    device_keys: DeviceKeysRecord,
    app_key: String,
    #[serde(default)]
    backup: BTreeMap<String, BackupPolicy>,
}

impl AlwaysOnConfig {
    fn identity(&self) -> Identity {
        Identity::new(&self.device_id, &self.app_id, &self.user_id)
    }

    fn device_keys(&self) -> Result<DeviceKeys, String> {
        self.device_keys.to_keys()
    }

    fn app_key(&self) -> Result<AppKey, String> {
        let bytes = BASE64
            .decode(self.app_key.as_bytes())
            .map_err(|error| error.to_string())?;
        AppKey::from_slice(&bytes).map_err(|error| error.to_string())
    }
}

struct AlwaysOnPaths {
    config: PathBuf,
    backups: PathBuf,
}

fn app_paths(app: &AppHandle) -> Result<AlwaysOnPaths, String> {
    let base = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    fs::create_dir_all(&base).map_err(|error| error.to_string())?;
    Ok(AlwaysOnPaths {
        config: base.join("config.json"),
        backups: base.join("backups"),
    })
}

fn load_or_init_config(path: &Path) -> Result<(AlwaysOnConfig, DeviceKeys, AppKey), String> {
    if path.exists() {
        let data = fs::read(path).map_err(|error| error.to_string())?;
        let config: AlwaysOnConfig =
            serde_json::from_slice(&data).map_err(|error| error.to_string())?;
        let keys = config.device_keys()?;
        let app_key = config.app_key()?;
        return Ok((config, keys, app_key));
    }

    let user_id = whoami::username();
    let device_id = format!("always-on-{}", Uuid::new_v4().simple());
    let app_id = "com.codedbydan.libresync".to_string();
    let identity = Identity::new(&device_id, &app_id, &user_id);
    let keys = DeviceKeys::generate(&identity).map_err(|error| error.to_string())?;
    let app_key = AppKey::generate().map_err(|error| error.to_string())?;

    let config = AlwaysOnConfig {
        app_id,
        device_id,
        user_id,
        device_keys: DeviceKeysRecord::from_keys(&keys),
        app_key: BASE64.encode(app_key.as_bytes()),
        backup: BTreeMap::new(),
    };

    save_config(path, &config)?;
    Ok((config, keys, app_key))
}

fn save_config(path: &Path, config: &AlwaysOnConfig) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(config).map_err(|error| error.to_string())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(path, data).map_err(|error| error.to_string())?;
    Ok(())
}

fn ensure_backup_policy(config: &mut AlwaysOnConfig, default_dir: &Path) -> Result<(), String> {
    let entry = config
        .backup
        .entry(config.app_id.clone())
        .or_insert_with(BackupPolicy::default);
    if entry.backup_dir.is_none() {
        entry.backup_dir = Some(default_dir.to_path_buf());
    }
    Ok(())
}
