#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::BTreeMap;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use libresync::{
    AppKey, AutoRefresh, AutoRefreshConfig, BackupManager, DataAdapter, DataAdapterBackup, DeviceKeys,
    Engine, EngineConfig, Event, EventStream, FileSnapshotStore, Identity, JsonFileAdapter,
    MdnsAdvertiser, RestoreOptions, SnapshotDiffSummary, SnapshotMetadata, State, summarize_snapshot_diff,
    register_mdns,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State as TauriState};
use uuid::Uuid;

const FILE_KEY: &str = "file";
const DEFAULT_LISTEN: &str = "0.0.0.0:52345";

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let paths = app_paths(app).map_err(|error| error.to_string())?;
            let (mut config, keys) = load_or_init_config(&paths.config, &paths.data)
                .map_err(|error| error.to_string())?;
            ensure_backup_policy(&mut config, &paths.backups)
                .map_err(|error| error.to_string())?;
            save_config(&paths.config, &config).map_err(|error| error.to_string())?;

            let app_key = config.app_key().map_err(|error| error.to_string())?;
            let identity = config.identity();
            let listen_addr = config.listen_addr().map_err(|error| error.to_string())?;
            let state = load_or_init_state(&config, &app_key)
                .map_err(|error| error.to_string())?;

            let config_arc = Arc::new(Mutex::new(config));
            let handler = Arc::new(AlwaysOnHandler {
                app_id: identity.app_id.clone(),
                keys,
                config: Arc::clone(&config_arc),
                config_path: paths.config.clone(),
            });

            let mut engine = Engine::new(
                EngineConfig::new(identity.clone()).with_listen_addr(listen_addr),
                state,
                handler,
            );

            if let Some(data_path) = config_arc
                .lock()
                .expect("config lock")
                .data_path
                .clone()
            {
                let adapter = Arc::new(JsonFileAdapter::new(FILE_KEY, data_path));
                engine
                    .register_adapter(adapter)
                    .map_err(|error| error.to_string())?;
            }

            let listener_addr = engine
                .start_listening()
                .map_err(|error| error.to_string())?;
            let mdns = register_mdns(&identity, listener_addr)
                .map_err(|error| error.to_string())?;

            let auto_refresh = if config_arc.lock().expect("config lock").data_path.is_some() {
                Some(
                    engine
                        .auto_refresh_with_config(
                            AutoRefreshConfig::new(FILE_KEY, paths.state)
                                .with_poll_interval(Duration::from_millis(500))
                                .with_refresh_interval(Duration::from_secs(10))
                                .with_discover_timeout(Duration::from_secs(2)),
                        )
                        .map_err(|error| error.to_string())?,
                )
            } else {
                None
            };

            let status = Arc::new(Mutex::new(AlwaysOnStatus::from_config(
                &config_arc.lock().expect("config lock"),
                listener_addr,
            )));
            let stream = engine.attach_event_channel();
            spawn_status_listener(stream, Arc::clone(&status));

            app.manage(AlwaysOnRuntime {
                engine: Mutex::new(engine),
                config: config_arc,
                config_path: paths.config,
                status,
                auto_refresh: Mutex::new(auto_refresh),
                mdns: Mutex::new(Some(mdns)),
                listener_addr,
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            manual_refresh,
            set_backup_policy,
            list_snapshots,
            preview_snapshot,
            restore_snapshot
        ])
        .run(tauri::generate_context!())
        .expect("error while running LibreSyncAlwaysOn");
}

#[derive(Clone)]
struct AlwaysOnRuntime {
    engine: Mutex<Engine>,
    config: Arc<Mutex<AlwaysOnConfig>>,
    config_path: PathBuf,
    status: Arc<Mutex<AlwaysOnStatus>>,
    auto_refresh: Mutex<Option<AutoRefresh>>,
    mdns: Mutex<Option<MdnsAdvertiser>>,
    listener_addr: SocketAddr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AlwaysOnStatus {
    device_id: String,
    user_id: String,
    app_id: String,
    listen_addr: String,
    paired_count: usize,
    last_sync_unix_secs: Option<u64>,
    last_error: Option<String>,
    backup: BTreeMap<String, BackupPolicy>,
    data_path: Option<PathBuf>,
}

impl AlwaysOnStatus {
    fn from_config(config: &AlwaysOnConfig, listen_addr: SocketAddr) -> Self {
        Self {
            device_id: config.device_id.clone(),
            user_id: config.user_id.clone(),
            app_id: config.app_id.clone(),
            listen_addr: listen_addr.to_string(),
            paired_count: config.devices.len(),
            last_sync_unix_secs: None,
            last_error: None,
            backup: config.backup.clone(),
            data_path: config.data_path.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RefreshSummary {
    attempted: usize,
    succeeded: usize,
    last_error: Option<String>,
}

#[tauri::command]
fn get_status(state: TauriState<AlwaysOnRuntime>) -> Result<AlwaysOnStatus, String> {
    let status = state.status.lock().map_err(|_| "status lock".to_string())?;
    Ok(status.clone())
}

#[tauri::command]
fn manual_refresh(state: TauriState<AlwaysOnRuntime>) -> Result<RefreshSummary, String> {
    let mut engine = state.engine.lock().map_err(|_| "engine lock".to_string())?;
    let config = state.config.lock().map_err(|_| "config lock".to_string())?;

    if config.data_path.is_none() {
        return Err("no data path configured for always-on refresh".to_string());
    }

    let state_path = config.state_path.clone();
    let app_key = config.app_key().map_err(|error| error.to_string())?;

    let devices = engine
        .discover_devices_with_timeout(Duration::from_secs(2))
        .map_err(|error| error.to_string())?;

    let mut attempted = 0;
    let mut succeeded = 0;
    let mut last_error = None;

    for device in devices {
        if !device.paired {
            continue;
        }
        if let Some(addr) = device.address {
            attempted += 1;
            match engine.sync_now(addr, FILE_KEY) {
                Ok(_) => succeeded += 1,
                Err(error) => last_error = Some(error.to_string()),
            }
        }
    }

    drop(config);

    if succeeded > 0 {
        let state_guard = engine.state();
        if let Ok(state_lock) = state_guard.lock() {
            let _ = state_lock.save_encrypted(&app_key, &state_path);
        }
    }

    let mut status = state.status.lock().map_err(|_| "status lock".to_string())?;
    if succeeded > 0 {
        status.last_sync_unix_secs = Some(now_unix_secs());
    }
    if let Some(error) = &last_error {
        status.last_error = Some(error.clone());
    }

    Ok(RefreshSummary {
        attempted,
        succeeded,
        last_error,
    })
}

#[tauri::command]
fn set_backup_policy(
    state: TauriState<AlwaysOnRuntime>,
    app_id: String,
    enabled: bool,
    allow_restore: bool,
) -> Result<AlwaysOnStatus, String> {
    let mut config = state.config.lock().map_err(|_| "config lock".to_string())?;
    let entry = config
        .backup
        .entry(app_id.clone())
        .or_insert_with(BackupPolicy::default);
    entry.enabled = enabled;
    entry.allow_restore = allow_restore;

    save_config(&state.config_path, &config)?;

    let mut status = state.status.lock().map_err(|_| "status lock".to_string())?;
    status.backup = config.backup.clone();
    Ok(status.clone())
}

#[tauri::command]
fn list_snapshots(state: TauriState<AlwaysOnRuntime>, app_id: String) -> Result<Vec<SnapshotMetadata>, String> {
    let (manager, _adapter_id, _policy) = backup_manager_for_app(&state, &app_id)?;
    let snapshots = manager
        .list_snapshots(FILE_KEY)
        .map_err(|error| error.to_string())?;
    Ok(snapshots)
}

#[tauri::command]
fn preview_snapshot(
    state: TauriState<AlwaysOnRuntime>,
    app_id: String,
    snapshot_id: String,
) -> Result<SnapshotDiffSummary, String> {
    let (manager, adapter, _policy) = backup_manager_for_app(&state, &app_id)?;
    let mut engine = state.engine.lock().map_err(|_| "engine lock".to_string())?;
    let mut state_lock = engine.state().lock().map_err(|_| "state lock".to_string())?;

    adapter
        .load_into_state(&mut state_lock)
        .map_err(|error| error.to_string())?;

    let (_metadata, entries) = manager
        .load_snapshot_entries(FILE_KEY, &snapshot_id)
        .map_err(|error| error.to_string())?;
    summarize_snapshot_diff(&state_lock, &entries).map_err(|error| error.to_string())
}

#[tauri::command]
fn restore_snapshot(
    state: TauriState<AlwaysOnRuntime>,
    app_id: String,
    snapshot_id: String,
    confirm_id: String,
) -> Result<AlwaysOnStatus, String> {
    if confirm_id != snapshot_id {
        return Err("confirm_id must match snapshot_id".to_string());
    }

    let (manager, adapter, policy) = backup_manager_for_app(&state, &app_id)?;
    if !policy.allow_restore {
        return Err("restores are not allowed for this app".to_string());
    }
    let adapter_arc: Arc<dyn DataAdapter> = Arc::new(adapter.clone());
    let backup_adapter = DataAdapterBackup::new(adapter_arc);
    let mut engine = state.engine.lock().map_err(|_| "engine lock".to_string())?;
    let mut state_lock = engine.state().lock().map_err(|_| "state lock".to_string())?;

    manager
        .restore_snapshot(
            &backup_adapter,
            &mut state_lock,
            &snapshot_id,
            RestoreOptions::confirmed(),
        )
        .map_err(|error| error.to_string())?;
    adapter
        .apply_from_state(&state_lock)
        .map_err(|error| error.to_string())?;

    let config = state.config.lock().map_err(|_| "config lock".to_string())?;
    let app_key = config.app_key().map_err(|error| error.to_string())?;
    state_lock
        .save_encrypted(&app_key, &config.state_path)
        .map_err(|error| error.to_string())?;
    drop(config);

    let mut status = state.status.lock().map_err(|_| "status lock".to_string())?;
    status.last_sync_unix_secs = Some(now_unix_secs());
    Ok(status.clone())
}

fn backup_manager_for_app(
    state: &TauriState<AlwaysOnRuntime>,
    app_id: &str,
) -> Result<(BackupManager, JsonFileAdapter, BackupPolicy), String> {
    let config = state.config.lock().map_err(|_| "config lock".to_string())?;
    let policy = config
        .backup
        .get(app_id)
        .cloned()
        .unwrap_or_default();
    if !policy.enabled {
        return Err("backups are not enabled for this app".to_string());
    }
    let backup_dir = policy
        .backup_dir
        .clone()
        .unwrap_or_else(|| state.config_path.parent().unwrap_or(Path::new(\".\")).join(\"backups\"));
    let store = FileSnapshotStore::new(&backup_dir).map_err(|error| error.to_string())?;
    let app_key = config.app_key().map_err(|error| error.to_string())?;
    let manager = BackupManager::new(app_key, Arc::new(store));
    let data_path = config
        .data_path
        .clone()
        .ok_or_else(|| \"no data path configured\".to_string())?;
    let adapter = JsonFileAdapter::new(FILE_KEY, data_path);
    Ok((manager, adapter, policy))
}

fn spawn_status_listener(stream: EventStream, status: Arc<Mutex<AlwaysOnStatus>>) {
    thread::spawn(move || loop {
        let event = match stream.recv() {
            Ok(event) => event,
            Err(_) => break,
        };

        match event {
            Event::SyncFinished { result, .. } => {
                let mut status = match status.lock() {
                    Ok(status) => status,
                    Err(_) => continue,
                };
                match result {
                    libresync::SyncResult::Success => {
                        status.last_sync_unix_secs = Some(now_unix_secs());
                    }
                    libresync::SyncResult::Failed(message) => {
                        status.last_error = Some(message);
                    }
                }
            }
            Event::Error { message } => {
                if let Ok(mut status) = status.lock() {
                    status.last_error = Some(message);
                }
            }
            _ => {}
        }
    });
}

struct AlwaysOnHandler {
    app_id: String,
    keys: DeviceKeys,
    config: Arc<Mutex<AlwaysOnConfig>>,
    config_path: PathBuf,
}

impl libresync::DeviceHandler for AlwaysOnHandler {
    fn app_id(&self) -> &str {
        &self.app_id
    }

    fn is_paired(&self, identity: &libresync::Identity) -> bool {
        let config = self.config.lock().expect("config lock");
        config.devices.contains_key(&identity.device_id)
    }

    fn approve_pair(&self, _identity: &libresync::Identity) -> libresync::Result<bool> {
        Ok(false)
    }

    fn device_keys(&self) -> libresync::Result<DeviceKeys> {
        Ok(self.keys.clone())
    }

    fn app_key(&self) -> libresync::Result<AppKey> {
        let config = self.config.lock().expect("config lock");
        config
            .app_key()
            .map_err(|error| libresync::Error::Protocol(error))
    }

    fn set_app_key(&self, app_key: &AppKey) -> libresync::Result<()> {
        let mut config = self.config.lock().expect("config lock");
        config.set_app_key(app_key);
        save_config(&self.config_path, &config)
            .map_err(|error| libresync::Error::Protocol(error))
    }

    fn is_paired_with_fingerprint(&self, identity: &libresync::Identity, fingerprint: &str) -> bool {
        if fingerprint.is_empty() {
            return self.is_paired(identity);
        }
        let config = self.config.lock().expect("config lock");
        config
            .devices
            .get(&identity.device_id)
            .and_then(|record| record.fingerprint.as_deref())
            .map(|stored| stored == fingerprint)
            .unwrap_or(false)
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceRecord {
    device_id: String,
    user_id: String,
    app_id: String,
    #[serde(default)]
    fingerprint: Option<String>,
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
    listen_addr: String,
    state_path: PathBuf,
    data_path: Option<PathBuf>,
    device_keys: DeviceKeysRecord,
    app_key: String,
    #[serde(default)]
    devices: BTreeMap<String, DeviceRecord>,
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

    fn set_app_key(&mut self, key: &AppKey) {
        self.app_key = BASE64.encode(key.as_bytes());
    }

    fn listen_addr(&self) -> Result<SocketAddr, String> {
        self.listen_addr
            .parse::<SocketAddr>()
            .map_err(|error| error.to_string())
    }
}

struct AlwaysOnPaths {
    config: PathBuf,
    backups: PathBuf,
    data: PathBuf,
    state: PathBuf,
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
        data: base.join("libresync.json"),
        state: base.join("state.json"),
    })
}

fn load_or_init_config(path: &Path, data_path: &Path) -> Result<(AlwaysOnConfig, DeviceKeys), String> {
    if path.exists() {
        let data = fs::read(path).map_err(|error| error.to_string())?;
        let config: AlwaysOnConfig =
            serde_json::from_slice(&data).map_err(|error| error.to_string())?;
        let keys = config.device_keys()?;
        return Ok((config, keys));
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
        listen_addr: DEFAULT_LISTEN.to_string(),
        state_path: data_path.with_file_name("state.json"),
        data_path: Some(data_path.to_path_buf()),
        device_keys: DeviceKeysRecord::from_keys(&keys),
        app_key: BASE64.encode(app_key.as_bytes()),
        devices: BTreeMap::new(),
        backup: BTreeMap::new(),
    };

    save_config(path, &config)?;
    Ok((config, keys))
}

fn load_or_init_state(config: &AlwaysOnConfig, app_key: &AppKey) -> Result<State, String> {
    if config.state_path.exists() {
        let state = State::load_maybe_encrypted(app_key, &config.state_path)
            .map_err(|error| error.to_string())?;
        if state.device_id != config.device_id {
            return Err("state device id does not match config".to_string());
        }
        Ok(state)
    } else {
        let state = State::new(config.device_id.clone());
        state
            .save_encrypted(app_key, &config.state_path)
            .map_err(|error| error.to_string())?;
        Ok(state)
    }
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

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
