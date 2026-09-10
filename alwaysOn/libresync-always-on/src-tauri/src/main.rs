#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use libresync::{
    register_mdns, summarize_snapshot_diff, AppKey, AutoRefresh, AutoRefreshConfig, BackupManager,
    DataAdapter, DataAdapterBackup, DeviceInfo, DeviceKeys, Engine, EngineConfig, Event,
    EventStream, FileSnapshotStore, Identity, JsonFileAdapter, MdnsAdvertiser, RestoreOptions,
    RetentionPolicy, SnapshotDiffSummary, SnapshotMetadata, State,
};
use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, CustomMenuItem, Manager, State as TauriState, SystemTray, SystemTrayEvent,
    SystemTrayMenu, SystemTrayMenuItem, WindowEvent,
};
use uuid::Uuid;

const FILE_KEY: &str = "file";
const DEFAULT_LISTEN: &str = "0.0.0.0:52345";
const AUTO_APPROVE_INTERVAL_SECS: u64 = 5;
const AUTO_APPROVE_RETRY_SECS: u64 = 300;
const AUTO_APPROVE_DISCOVER_TIMEOUT_SECS: u64 = 2;

fn main() {
    let show_item = CustomMenuItem::new("show".to_string(), "Show");
    let hide_item = CustomMenuItem::new("hide".to_string(), "Hide");
    let refresh_item = CustomMenuItem::new("refresh".to_string(), "Manual refresh");
    let quit_item = CustomMenuItem::new("quit".to_string(), "Quit");
    let tray_menu = SystemTrayMenu::new()
        .add_item(show_item)
        .add_item(hide_item)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(refresh_item)
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(quit_item);

    tauri::Builder::default()
        .system_tray(SystemTray::new().with_menu(tray_menu))
        .setup(|app| {
            let paths = app_paths(&app.handle()).map_err(|error| error.to_string())?;
            let (mut config, keys) = load_or_init_config(&paths.config, &paths.data)
                .map_err(|error| error.to_string())?;
            ensure_backup_policy(&mut config, &paths.backups).map_err(|error| error.to_string())?;
            save_config(&paths.config, &config).map_err(|error| error.to_string())?;

            let app_key = config.app_key().map_err(|error| error.to_string())?;
            let identity = config.identity();
            let listen_addr = config.listen_addr().map_err(|error| error.to_string())?;
            let state = load_or_init_state(&config, &app_key).map_err(|error| error.to_string())?;

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

            if let Some(data_path) = config_arc.lock().expect("config lock").data_path.clone() {
                let adapter = Arc::new(JsonFileAdapter::new(FILE_KEY, data_path));
                engine
                    .register_adapter(adapter)
                    .map_err(|error| error.to_string())?;
            }

            let listener_addr = engine
                .start_listening()
                .map_err(|error| error.to_string())?;
            let mdns =
                register_mdns(&identity, listener_addr).map_err(|error| error.to_string())?;

            let auto_refresh = if config_arc.lock().expect("config lock").data_path.is_some() {
                let fallback_addresses = config_arc
                    .lock()
                    .expect("config lock")
                    .devices
                    .values()
                    .filter_map(|record| {
                        record
                            .last_seen_addr
                            .as_deref()
                            .and_then(|addr| addr.parse::<SocketAddr>().ok())
                    })
                    .collect::<Vec<_>>();
                Some(
                    engine
                        .auto_refresh_with_config(
                            AutoRefreshConfig::new(FILE_KEY, paths.state)
                                .with_poll_interval(Duration::from_millis(500))
                                .with_refresh_interval(Duration::from_secs(10))
                                .with_discover_timeout(Duration::from_secs(2))
                                .with_fallback_addresses(fallback_addresses),
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

            let engine = Arc::new(Mutex::new(engine));
            spawn_auto_approve_loop(
                Arc::clone(&engine),
                Arc::clone(&config_arc),
                paths.config.clone(),
                Arc::clone(&status),
            );

            app.manage(AlwaysOnRuntime {
                engine,
                config: config_arc,
                config_path: paths.config,
                status,
                _auto_refresh: Mutex::new(auto_refresh),
                _mdns: Mutex::new(Some(mdns)),
                _listener_addr: listener_addr,
            });

            Ok(())
        })
        .on_system_tray_event(|app, event| match event {
            SystemTrayEvent::MenuItemClick { id, .. } => match id.as_str() {
                "show" => {
                    if let Some(window) = app.get_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
                "hide" => {
                    if let Some(window) = app.get_window("main") {
                        let _ = window.hide();
                    }
                }
                "refresh" => {
                    if let Some(state) = app.try_state::<AlwaysOnRuntime>() {
                        let _ = manual_refresh_internal(&state);
                    }
                }
                "quit" => {
                    std::process::exit(0);
                }
                _ => {}
            },
            _ => {}
        })
        .on_window_event(|event| {
            if let WindowEvent::CloseRequested { api, .. } = event.event() {
                let _ = event.window().hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_status,
            manual_refresh,
            link_device,
            unlink_device,
            set_auto_accept_linking,
            set_auto_approve_linking,
            set_pairing_secret,
            set_backup_policy,
            create_snapshot,
            list_snapshots,
            preview_snapshot,
            restore_snapshot,
            prune_snapshots
        ])
        .run(tauri::generate_context!())
        .expect("error while running LibreSyncAlwaysOn");
}

struct AlwaysOnRuntime {
    engine: Arc<Mutex<Engine>>,
    config: Arc<Mutex<AlwaysOnConfig>>,
    config_path: PathBuf,
    status: Arc<Mutex<AlwaysOnStatus>>,
    _auto_refresh: Mutex<Option<AutoRefresh>>,
    _mdns: Mutex<Option<MdnsAdvertiser>>,
    _listener_addr: SocketAddr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AlwaysOnStatus {
    device_id: String,
    user_id: String,
    app_id: String,
    listen_addr: String,
    linked_count: usize,
    linked_devices: Vec<DeviceRecord>,
    local_fingerprint: String,
    auto_accept_linking: bool,
    auto_approve_linking: bool,
    auto_approve_until: Option<u64>,
    pairing_secret_set: bool,
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
            linked_count: config.devices.len(),
            linked_devices: config.devices.values().cloned().collect(),
            local_fingerprint: config.device_keys.fingerprint.clone(),
            auto_accept_linking: config.auto_accept_linking,
            auto_approve_linking: config.auto_approve_linking,
            auto_approve_until: config.auto_approve_until,
            pairing_secret_set: config.pairing_secret.is_some(),
            last_sync_unix_secs: None,
            last_error: None,
            backup: config.backup.clone(),
            data_path: config.data_path.clone(),
        }
    }
}

fn sync_status_with_config(status: &mut AlwaysOnStatus, config: &AlwaysOnConfig) {
    status.device_id = config.device_id.clone();
    status.user_id = config.user_id.clone();
    status.app_id = config.app_id.clone();
    status.listen_addr = config.listen_addr.clone();
    status.linked_devices = config.devices.values().cloned().collect();
    status.linked_count = status.linked_devices.len();
    status.local_fingerprint = config.device_keys.fingerprint.clone();
    status.auto_accept_linking = config.auto_accept_linking;
    status.auto_approve_linking = config.auto_approve_linking;
    status.auto_approve_until = config.auto_approve_until;
    status.pairing_secret_set = config.pairing_secret.is_some();
    status.backup = config.backup.clone();
    status.data_path = config.data_path.clone();
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RefreshSummary {
    attempted: usize,
    succeeded: usize,
    last_error: Option<String>,
}

#[tauri::command]
fn get_status(state: TauriState<AlwaysOnRuntime>) -> Result<AlwaysOnStatus, String> {
    let config = state.config.lock().map_err(|_| "config lock".to_string())?;
    let mut status = state.status.lock().map_err(|_| "status lock".to_string())?;
    sync_status_with_config(&mut status, &config);
    Ok(status.clone())
}

fn manual_refresh_internal(state: &AlwaysOnRuntime) -> Result<RefreshSummary, String> {
    let engine = state.engine.lock().map_err(|_| "engine lock".to_string())?;
    let (state_path, app_key, fallback_records) = {
        let config = state.config.lock().map_err(|_| "config lock".to_string())?;
        if config.data_path.is_none() {
            return Err("no data path configured for always-on refresh".to_string());
        }
        (
            config.state_path.clone(),
            config.app_key().map_err(|error| error.to_string())?,
            config.devices.values().cloned().collect::<Vec<_>>(),
        )
    };

    let devices = engine
        .discover_devices_with_timeout(Duration::from_secs(2))
        .map_err(|error| error.to_string())?;

    let mut attempted = 0;
    let mut succeeded = 0;
    let mut last_error = None;

    let mut address_book = Vec::new();
    for device in devices {
        if !device.linked {
            continue;
        }
        if let Some(addr) = device.address {
            address_book.push((device, addr));
        }
    }

    if address_book.is_empty() {
        for record in fallback_records {
            if let Some(addr) = record.last_seen_addr.as_deref() {
                if let Ok(parsed) = addr.parse::<SocketAddr>() {
                    let device = DeviceInfo {
                        identity: Identity::new(&record.device_id, &record.app_id, &record.user_id),
                        address: Some(parsed),
                        last_seen: None,
                        linked: true,
                        fingerprint: record.fingerprint.clone(),
                    };
                    address_book.push((device, parsed));
                }
            }
        }
    }

    for (_device, addr) in address_book {
        attempted += 1;
        match engine.sync_now(addr, FILE_KEY) {
            Ok(remote) => {
                succeeded += 1;
                let mut config = state.config.lock().map_err(|_| "config lock".to_string())?;
                config.upsert_device(&remote.identity, Some(addr), remote.fingerprint.clone());
                save_config(&state.config_path, &config)?;
            }
            Err(error) => last_error = Some(error.to_string()),
        }
    }

    if succeeded > 0 {
        let state_guard = engine.state();
        if let Ok(state_lock) = state_guard.lock() {
            let _ = state_lock.save_encrypted(&app_key, &state_path);
        };
    }

    let mut status = state.status.lock().map_err(|_| "status lock".to_string())?;
    if succeeded > 0 {
        status.last_sync_unix_secs = Some(now_unix_secs());
        status.last_error = None;
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
fn manual_refresh(state: TauriState<AlwaysOnRuntime>) -> Result<RefreshSummary, String> {
    manual_refresh_internal(&state)
}

#[tauri::command]
fn link_device(
    state: TauriState<AlwaysOnRuntime>,
    address: String,
) -> Result<AlwaysOnStatus, String> {
    let addr = address
        .parse::<SocketAddr>()
        .map_err(|_| "invalid address".to_string())?;
    let engine = state.engine.lock().map_err(|_| "engine lock".to_string())?;
    let remote = engine
        .request_link(addr)
        .map_err(|error| error.to_string())?;

    let mut config = state.config.lock().map_err(|_| "config lock".to_string())?;
    config.upsert_device(&remote.identity, Some(addr), remote.fingerprint.clone());
    save_config(&state.config_path, &config)?;

    let mut status = state.status.lock().map_err(|_| "status lock".to_string())?;
    sync_status_with_config(&mut status, &config);
    Ok(status.clone())
}

#[tauri::command]
fn unlink_device(
    state: TauriState<AlwaysOnRuntime>,
    device_id: String,
) -> Result<AlwaysOnStatus, String> {
    let mut config = state.config.lock().map_err(|_| "config lock".to_string())?;
    config.devices.remove(&device_id);
    save_config(&state.config_path, &config)?;

    let mut status = state.status.lock().map_err(|_| "status lock".to_string())?;
    sync_status_with_config(&mut status, &config);
    Ok(status.clone())
}

#[tauri::command]
fn set_auto_accept_linking(
    state: TauriState<AlwaysOnRuntime>,
    enabled: bool,
) -> Result<AlwaysOnStatus, String> {
    let mut config = state.config.lock().map_err(|_| "config lock".to_string())?;
    config.auto_accept_linking = enabled;
    if !enabled {
        config.auto_approve_linking = false;
        config.auto_approve_until = None;
    }
    save_config(&state.config_path, &config)?;

    let mut status = state.status.lock().map_err(|_| "status lock".to_string())?;
    sync_status_with_config(&mut status, &config);
    Ok(status.clone())
}

#[tauri::command]
fn set_auto_approve_linking(
    state: TauriState<AlwaysOnRuntime>,
    enabled: bool,
    duration_mins: Option<u64>,
) -> Result<AlwaysOnStatus, String> {
    let mut config = state.config.lock().map_err(|_| "config lock".to_string())?;
    config.auto_approve_linking = enabled;
    if enabled {
        config.auto_accept_linking = true;
        let minutes = duration_mins.unwrap_or(15).max(1);
        config.auto_approve_until = Some(now_unix_secs() + minutes.saturating_mul(60));
    } else {
        config.auto_approve_until = None;
    }
    save_config(&state.config_path, &config)?;

    let mut status = state.status.lock().map_err(|_| "status lock".to_string())?;
    sync_status_with_config(&mut status, &config);
    Ok(status.clone())
}

#[tauri::command]
fn set_pairing_secret(
    state: TauriState<AlwaysOnRuntime>,
    secret: Option<String>,
) -> Result<AlwaysOnStatus, String> {
    let mut config = state.config.lock().map_err(|_| "config lock".to_string())?;
    config.pairing_secret = secret.filter(|value| !value.trim().is_empty());
    save_config(&state.config_path, &config)?;

    let mut status = state.status.lock().map_err(|_| "status lock".to_string())?;
    sync_status_with_config(&mut status, &config);
    Ok(status.clone())
}

#[tauri::command]
fn create_snapshot(
    state: TauriState<AlwaysOnRuntime>,
    app_id: String,
    note: Option<String>,
) -> Result<SnapshotMetadata, String> {
    let (manager, adapter, policy) = backup_manager_for_app(&state, &app_id)?;
    let adapter_arc: Arc<dyn DataAdapter> = Arc::new(adapter.clone());
    let backup_adapter = DataAdapterBackup::new(adapter_arc);
    let engine = state.engine.lock().map_err(|_| "engine lock".to_string())?;
    let state_guard = engine.state();
    let mut state_lock = state_guard.lock().map_err(|_| "state lock".to_string())?;

    adapter
        .load_into_state(&mut state_lock)
        .map_err(|error| error.to_string())?;

    let metadata = manager
        .create_snapshot(&backup_adapter, &state_lock, note)
        .map_err(|error| error.to_string())?;

    if !policy.retention_policy().is_empty() {
        let _ = manager.prune_snapshots(FILE_KEY, policy.retention_policy());
    }

    let config = state.config.lock().map_err(|_| "config lock".to_string())?;
    let app_key = config.app_key().map_err(|error| error.to_string())?;
    state_lock
        .save_encrypted(&app_key, &config.state_path)
        .map_err(|error| error.to_string())?;

    Ok(metadata)
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
    if entry.max_snapshots.is_none() {
        entry.max_snapshots = Some(20);
    }
    if entry.max_age_days.is_none() {
        entry.max_age_days = Some(30);
    }

    save_config(&state.config_path, &config)?;

    let mut status = state.status.lock().map_err(|_| "status lock".to_string())?;
    sync_status_with_config(&mut status, &config);
    Ok(status.clone())
}

#[tauri::command]
fn list_snapshots(
    state: TauriState<AlwaysOnRuntime>,
    app_id: String,
) -> Result<Vec<SnapshotMetadata>, String> {
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
    let engine = state.engine.lock().map_err(|_| "engine lock".to_string())?;
    let state_guard = engine.state();
    let mut state_lock = state_guard.lock().map_err(|_| "state lock".to_string())?;

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
    let engine = state.engine.lock().map_err(|_| "engine lock".to_string())?;
    let state_guard = engine.state();
    let mut state_lock = state_guard.lock().map_err(|_| "state lock".to_string())?;

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

#[tauri::command]
fn prune_snapshots(state: TauriState<AlwaysOnRuntime>, app_id: String) -> Result<usize, String> {
    let (manager, _adapter, policy) = backup_manager_for_app(&state, &app_id)?;
    let retention = policy.retention_policy();
    if retention.is_empty() {
        return Ok(0);
    }
    let summary = manager
        .prune_snapshots(FILE_KEY, retention)
        .map_err(|error| error.to_string())?;
    Ok(summary.deleted.len())
}

fn backup_manager_for_app(
    state: &TauriState<AlwaysOnRuntime>,
    app_id: &str,
) -> Result<(BackupManager, JsonFileAdapter, BackupPolicy), String> {
    let config = state.config.lock().map_err(|_| "config lock".to_string())?;
    let policy = config.backup.get(app_id).cloned().unwrap_or_default();
    if !policy.enabled {
        return Err("backups are not enabled for this app".to_string());
    }
    let backup_dir = policy.backup_dir.clone().unwrap_or_else(|| {
        state
            .config_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("backups")
    });
    let store = FileSnapshotStore::new(&backup_dir).map_err(|error| error.to_string())?;
    let app_key = config.app_key().map_err(|error| error.to_string())?;
    let manager = BackupManager::new(app_key, Arc::new(store));
    let data_path = config
        .data_path
        .clone()
        .ok_or_else(|| "no data path configured".to_string())?;
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
                        status.last_error = None;
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

fn set_status_error(status: &Arc<Mutex<AlwaysOnStatus>>, message: String) {
    if let Ok(mut status) = status.lock() {
        status.last_error = Some(message);
    }
}

fn spawn_auto_approve_loop(
    engine: Arc<Mutex<Engine>>,
    config: Arc<Mutex<AlwaysOnConfig>>,
    config_path: PathBuf,
    status: Arc<Mutex<AlwaysOnStatus>>,
) {
    thread::spawn(move || {
        let mut attempts: HashMap<String, Instant> = HashMap::new();
        let mut last_run = Instant::now()
            .checked_sub(Duration::from_secs(AUTO_APPROVE_INTERVAL_SECS))
            .unwrap_or_else(Instant::now);

        loop {
            thread::sleep(Duration::from_millis(500));
            if last_run.elapsed() < Duration::from_secs(AUTO_APPROVE_INTERVAL_SECS) {
                continue;
            }

            let enabled = match config.lock() {
                Ok(mut config) => {
                    let (active, expired) = config.auto_approve_state();
                    if expired {
                        let _ = save_config(&config_path, &config);
                    }
                    active
                }
                Err(_) => false,
            };
            if !enabled {
                last_run = Instant::now();
                continue;
            }

            let devices = {
                let engine = match engine.lock() {
                    Ok(engine) => engine,
                    Err(_) => {
                        last_run = Instant::now();
                        continue;
                    }
                };
                match engine.discover_devices_with_timeout(Duration::from_secs(
                    AUTO_APPROVE_DISCOVER_TIMEOUT_SECS,
                )) {
                    Ok(devices) => devices,
                    Err(error) => {
                        set_status_error(&status, format!("Auto-approve discovery error: {error}"));
                        last_run = Instant::now();
                        continue;
                    }
                }
            };

            let now = Instant::now();
            for device in devices {
                let addr = match device.address {
                    Some(addr) => addr,
                    None => continue,
                };
                if !is_auto_approve_address(addr) {
                    continue;
                }
                let device_id = device.identity.device_id.clone();
                let already_linked = match config.lock() {
                    Ok(config) => config.devices.contains_key(&device_id),
                    Err(_) => true,
                };
                if already_linked {
                    continue;
                }
                if let Some(last) = attempts.get(&device_id) {
                    if now.duration_since(*last) < Duration::from_secs(AUTO_APPROVE_RETRY_SECS) {
                        continue;
                    }
                }

                let result = {
                    let engine = match engine.lock() {
                        Ok(engine) => engine,
                        Err(_) => {
                            attempts.insert(device_id, now);
                            continue;
                        }
                    };
                    engine.request_link(addr)
                };

                match result {
                    Ok(remote) => {
                        if let Ok(mut config) = config.lock() {
                            config.upsert_device(&remote.identity, Some(addr), remote.fingerprint);
                            let _ = save_config(&config_path, &config);
                        }
                        attempts.remove(&device_id);
                        if let Ok(mut status) = status.lock() {
                            status.last_error = None;
                        }
                        println!(
                            "Auto-approved link with {} ({})",
                            remote.identity.device_id, remote.identity.user_id
                        );
                    }
                    Err(error) => {
                        attempts.insert(device_id, now);
                        set_status_error(&status, format!("Auto-approve link failed: {error}"));
                    }
                }
            }

            last_run = Instant::now();
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

    fn is_linked(&self, identity: &libresync::Identity) -> bool {
        let config = self.config.lock().expect("config lock");
        config.devices.contains_key(&identity.device_id)
    }

    fn approve_link(&self, _identity: &libresync::Identity) -> libresync::Result<bool> {
        let config = self.config.lock().expect("config lock");
        Ok(config.auto_accept_linking)
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
        save_config(&self.config_path, &config).map_err(|error| libresync::Error::Protocol(error))
    }

    fn is_linked_with_fingerprint(
        &self,
        identity: &libresync::Identity,
        fingerprint: &str,
    ) -> bool {
        if fingerprint.is_empty() {
            return self.is_linked(identity);
        }
        let config = self.config.lock().expect("config lock");
        config
            .devices
            .get(&identity.device_id)
            .and_then(|record| record.fingerprint.as_deref())
            .map(|stored| stored == fingerprint)
            .unwrap_or(false)
    }

    fn pairing_secret(&self) -> Option<String> {
        self.config
            .lock()
            .ok()
            .and_then(|config| config.pairing_secret.clone())
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
    #[serde(default)]
    last_seen_addr: Option<String>,
    #[serde(default)]
    last_seen_unix_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BackupPolicy {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    allow_restore: bool,
    #[serde(default)]
    backup_dir: Option<PathBuf>,
    #[serde(default)]
    max_snapshots: Option<usize>,
    #[serde(default)]
    max_age_days: Option<u64>,
}

impl BackupPolicy {
    fn retention_policy(&self) -> RetentionPolicy {
        RetentionPolicy {
            max_snapshots: self.max_snapshots,
            max_age_secs: self
                .max_age_days
                .map(|days| days.saturating_mul(24 * 60 * 60)),
        }
    }
}

impl Default for BackupPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_restore: false,
            backup_dir: None,
            max_snapshots: Some(20),
            max_age_days: Some(30),
        }
    }
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
    auto_accept_linking: bool,
    #[serde(default)]
    auto_approve_linking: bool,
    #[serde(default)]
    auto_approve_until: Option<u64>,
    #[serde(default)]
    pairing_secret: Option<String>,
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

    fn upsert_device(
        &mut self,
        identity: &Identity,
        addr: Option<SocketAddr>,
        fingerprint: Option<String>,
    ) {
        let entry = self
            .devices
            .entry(identity.device_id.clone())
            .or_insert(DeviceRecord {
                device_id: identity.device_id.clone(),
                user_id: identity.user_id.clone(),
                app_id: identity.app_id.clone(),
                fingerprint: None,
                last_seen_addr: None,
                last_seen_unix_secs: None,
            });
        entry.user_id = identity.user_id.clone();
        entry.app_id = identity.app_id.clone();
        if let Some(addr) = addr {
            entry.last_seen_addr = Some(addr.to_string());
            entry.last_seen_unix_secs = Some(now_unix_secs());
        }
        if let Some(fingerprint) = fingerprint {
            entry.fingerprint = Some(fingerprint);
        }
    }

    fn auto_approve_state(&mut self) -> (bool, bool) {
        if !self.auto_approve_linking {
            return (false, false);
        }
        if let Some(until) = self.auto_approve_until {
            if now_unix_secs() > until {
                self.auto_approve_linking = false;
                self.auto_approve_until = None;
                return (false, true);
            }
        }
        (true, false)
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
        .path_resolver()
        .app_data_dir()
        .ok_or_else(|| "could not resolve app data directory".to_string())?;
    fs::create_dir_all(&base).map_err(|error| error.to_string())?;
    Ok(AlwaysOnPaths {
        config: base.join("config.json"),
        backups: base.join("backups"),
        data: base.join("libresync.json"),
        state: base.join("state.json"),
    })
}

fn load_or_init_config(
    path: &Path,
    data_path: &Path,
) -> Result<(AlwaysOnConfig, DeviceKeys), String> {
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
        auto_accept_linking: false,
        auto_approve_linking: false,
        auto_approve_until: None,
        pairing_secret: None,
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
    if entry.max_snapshots.is_none() {
        entry.max_snapshots = Some(20);
    }
    if entry.max_age_days.is_none() {
        entry.max_age_days = Some(30);
    }
    Ok(())
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn is_auto_approve_address(addr: SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(ip) => ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_unique_local() || ip.is_unicast_link_local(),
    }
}
