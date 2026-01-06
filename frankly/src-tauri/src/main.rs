use std::collections::BTreeMap;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use libresync::{
    AppKey, register_mdns, AutoRefresh, DeviceHandler, DeviceInfo, DeviceKeys, Engine,
    EngineConfig, Event, EventStream, Identity, MdnsAdvertiser, SqliteFileAdapter, State,
};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State as TauriState};
use uuid::Uuid;

const APP_ID: &str = "com.codedbydan.Frankly";
const TODOS_KEY: &str = "todos";
const LISTEN_ADDR: &str = "0.0.0.0:0";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Todo {
    id: String,
    title: String,
    completed: bool,
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
            .map_err(|error| format!("failed to decode cert: {error}"))?;
        let key_der = BASE64
            .decode(self.key_der.as_bytes())
            .map_err(|error| format!("failed to decode key: {error}"))?;
        DeviceKeys::from_der(cert_der, key_der).map_err(|error| error.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceRecord {
    device_id: String,
    user_id: String,
    app_id: String,
    last_seen_addr: Option<String>,
    last_seen_unix_secs: Option<u64>,
    fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FranklyConfig {
    app_id: String,
    device_id: String,
    user_id: String,
    device_keys: DeviceKeysRecord,
    #[serde(default)]
    app_key: Option<String>,
    #[serde(default)]
    devices: BTreeMap<String, DeviceRecord>,
}

impl FranklyConfig {
    fn identity(&self) -> Identity {
        Identity::new(&self.device_id, &self.app_id, &self.user_id)
    }

    fn device_keys(&self) -> Result<DeviceKeys, String> {
        self.device_keys.to_keys()
    }

    fn app_key(&self) -> Result<AppKey, String> {
        let encoded = self
            .app_key
            .as_ref()
            .ok_or_else(|| "app key missing".to_string())?;
        let bytes = BASE64
            .decode(encoded.as_bytes())
            .map_err(|error| error.to_string())?;
        AppKey::from_slice(&bytes).map_err(|error| error.to_string())
    }

    fn set_app_key(&mut self, app_key: &AppKey) {
        self.app_key = Some(BASE64.encode(app_key.as_bytes()));
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
                last_seen_addr: None,
                last_seen_unix_secs: None,
                fingerprint: None,
            });

        entry.user_id = identity.user_id.clone();
        entry.app_id = identity.app_id.clone();
        if let Some(addr) = addr {
            entry.last_seen_addr = Some(addr.to_string());
        }
        entry.last_seen_unix_secs = Some(unix_secs());
        if let Some(fingerprint) = fingerprint {
            entry.fingerprint = Some(fingerprint);
        }
    }
}

struct FranklyHandler {
    app_id: String,
    keys: DeviceKeys,
    config: Arc<Mutex<FranklyConfig>>,
    config_path: PathBuf,
    auto_accept: bool,
}

impl FranklyHandler {
    fn new(
        app_id: String,
        keys: DeviceKeys,
        config: Arc<Mutex<FranklyConfig>>,
        config_path: PathBuf,
    ) -> Self {
        Self {
            app_id,
            keys,
            config,
            config_path,
            auto_accept: true,
        }
    }
}

impl DeviceHandler for FranklyHandler {
    fn app_id(&self) -> &str {
        &self.app_id
    }

    fn is_paired(&self, identity: &Identity) -> bool {
        self.config
            .lock()
            .map(|config| config.devices.contains_key(&identity.device_id))
            .unwrap_or(false)
    }

    fn approve_pair(&self, identity: &Identity) -> libresync::Result<bool> {
        self.approve_pair_with_fingerprint(identity, "")
    }

    fn device_keys(&self) -> libresync::Result<DeviceKeys> {
        Ok(self.keys.clone())
    }

    fn app_key(&self) -> libresync::Result<AppKey> {
        self.config
            .lock()
            .map_err(|_| libresync::Error::Protocol("config lock poisoned".to_string()))?
            .app_key()
            .map_err(|error| libresync::Error::Protocol(error))
    }

    fn set_app_key(&self, app_key: &AppKey) -> libresync::Result<()> {
        let mut config = self
            .config
            .lock()
            .map_err(|_| libresync::Error::Protocol("config lock poisoned".to_string()))?;
        config.set_app_key(app_key);
        save_config(&self.config_path, &config)
            .map_err(|error| libresync::Error::Protocol(error))?;
        Ok(())
    }

    fn is_paired_with_fingerprint(&self, identity: &Identity, fingerprint: &str) -> bool {
        if fingerprint.is_empty() {
            return false;
        }
        self.config
            .lock()
            .map(|config| {
                config
                    .devices
                    .get(&identity.device_id)
                    .and_then(|record| record.fingerprint.as_deref())
                    .map(|stored| stored == fingerprint)
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }

    fn approve_pair_with_fingerprint(
        &self,
        identity: &Identity,
        fingerprint: &str,
    ) -> libresync::Result<bool> {
        if !self.auto_accept {
            return Ok(false);
        }

        let mut config = self
            .config
            .lock()
            .map_err(|_| libresync::Error::Protocol("config lock poisoned".to_string()))?;
        let fingerprint = if fingerprint.is_empty() {
            None
        } else {
            Some(fingerprint.to_string())
        };
        config.upsert_device(identity, None, fingerprint);
        save_config(&self.config_path, &config)
            .map_err(|error| libresync::Error::Protocol(error))?;

        Ok(true)
    }
}

struct FranklyState {
    engine: Mutex<Engine>,
    config: Arc<Mutex<FranklyConfig>>,
    config_path: PathBuf,
    state_path: PathBuf,
    db_path: PathBuf,
    listener_addr: Mutex<Option<SocketAddr>>,
    mdns: Mutex<Option<MdnsAdvertiser>>,
    auto_refresh: Mutex<Option<AutoRefresh>>,
}

#[derive(Debug, Serialize)]
struct AppStatus {
    app_id: String,
    device_id: String,
    user_id: String,
    listener_addr: Option<String>,
    linked_devices: usize,
    fingerprint: String,
}

#[derive(Debug, Serialize)]
struct DeviceInfoDto {
    device_id: String,
    user_id: String,
    app_id: String,
    linked: bool,
    address: Option<String>,
    last_seen_unix_secs: Option<u64>,
    fingerprint: Option<String>,
}

#[derive(Debug, Serialize)]
struct SyncResultDto {
    device: DeviceInfoDto,
    todos: Vec<Todo>,
}

#[derive(Debug, Serialize)]
struct SyncEventDto {
    kind: String,
    adapter_id: Option<String>,
    device_id: Option<String>,
    user_id: Option<String>,
    result: Option<String>,
    message: Option<String>,
}

#[tauri::command]
fn app_status(state: TauriState<FranklyState>) -> Result<AppStatus, String> {
    let config = state
        .config
        .lock()
        .map_err(|_| "config lock poisoned".to_string())?;
    let listener_addr = state
        .listener_addr
        .lock()
        .map_err(|_| "listener lock poisoned".to_string())?;

    Ok(AppStatus {
        app_id: config.app_id.clone(),
        device_id: config.device_id.clone(),
        user_id: config.user_id.clone(),
        listener_addr: listener_addr.as_ref().map(|addr| addr.to_string()),
        linked_devices: config.devices.len(),
        fingerprint: config.device_keys.fingerprint.clone(),
    })
}

#[tauri::command]
fn list_devices(state: TauriState<FranklyState>) -> Result<Vec<DeviceInfoDto>, String> {
    let config = state
        .config
        .lock()
        .map_err(|_| "config lock poisoned".to_string())?;
    Ok(config.devices.values().map(record_to_dto).collect())
}

#[tauri::command]
fn discover_devices(
    state: TauriState<FranklyState>,
    timeout_secs: Option<u64>,
) -> Result<Vec<DeviceInfoDto>, String> {
    let timeout = Duration::from_secs(timeout_secs.unwrap_or(3));
    let engine = state
        .engine
        .lock()
        .map_err(|_| "engine lock poisoned".to_string())?;
    let devices = engine
        .discover_devices_with_timeout(timeout)
        .map_err(|error| error.to_string())?;
    Ok(devices.into_iter().map(device_to_dto).collect())
}

#[tauri::command]
fn link_device(state: TauriState<FranklyState>, address: String) -> Result<DeviceInfoDto, String> {
    let addr = parse_addr(&address)?;
    let device = {
        let engine = state
            .engine
            .lock()
            .map_err(|_| "engine lock poisoned".to_string())?;
        engine
            .request_pair(addr)
            .map_err(|error| error.to_string())?
    };

    update_config_device(&state, &device)?;
    Ok(device_to_dto(device))
}

#[tauri::command]
fn sync_device(state: TauriState<FranklyState>, address: String) -> Result<SyncResultDto, String> {
    let addr = parse_addr(&address)?;
    let device = {
        let engine = state
            .engine
            .lock()
            .map_err(|_| "engine lock poisoned".to_string())?;
        engine
            .sync_now(addr, TODOS_KEY)
            .map_err(|error| error.to_string())?
    };

    update_config_device(&state, &device)?;
    let todos = load_todos(&state.db_path)?;

    Ok(SyncResultDto {
        device: device_to_dto(device),
        todos,
    })
}

#[tauri::command]
fn revoke_device(state: TauriState<FranklyState>, device_id: String) -> Result<(), String> {
    let mut config = state
        .config
        .lock()
        .map_err(|_| "config lock poisoned".to_string())?;
    if config.devices.remove(&device_id).is_none() {
        return Err(format!("unknown device: {device_id}"));
    }
    save_config(&state.config_path, &config)?;
    Ok(())
}

#[tauri::command]
fn get_todos(state: TauriState<FranklyState>) -> Result<Vec<Todo>, String> {
    load_todos(&state.db_path)
}

#[tauri::command]
fn add_todo(state: TauriState<FranklyState>, title: String) -> Result<Vec<Todo>, String> {
    let todo = Todo {
        id: Uuid::new_v4().simple().to_string(),
        title,
        completed: false,
    };
    let conn = open_db(&state.db_path)?;
    conn.execute(
        "INSERT INTO todos (id, title, completed) VALUES (?1, ?2, ?3)",
        params![todo.id, todo.title, todo.completed as i32],
    )
    .map_err(|error| error.to_string())?;
    load_todos(&state.db_path)
}

#[tauri::command]
fn update_todo(
    state: TauriState<FranklyState>,
    id: String,
    title: String,
    completed: bool,
) -> Result<Vec<Todo>, String> {
    let conn = open_db(&state.db_path)?;
    conn.execute(
        "UPDATE todos SET title = ?1, completed = ?2 WHERE id = ?3",
        params![title, completed as i32, id],
    )
    .map_err(|error| error.to_string())?;
    load_todos(&state.db_path)
}

#[tauri::command]
fn delete_todo(state: TauriState<FranklyState>, id: String) -> Result<Vec<Todo>, String> {
    let conn = open_db(&state.db_path)?;
    conn.execute("DELETE FROM todos WHERE id = ?1", params![id])
        .map_err(|error| error.to_string())?;
    load_todos(&state.db_path)
}

fn parse_addr(input: &str) -> Result<SocketAddr, String> {
    input
        .parse::<SocketAddr>()
        .map_err(|_| format!("invalid address: {input}"))
}

fn record_to_dto(record: &DeviceRecord) -> DeviceInfoDto {
    DeviceInfoDto {
        device_id: record.device_id.clone(),
        user_id: record.user_id.clone(),
        app_id: record.app_id.clone(),
        linked: true,
        address: record.last_seen_addr.clone(),
        last_seen_unix_secs: record.last_seen_unix_secs,
        fingerprint: record.fingerprint.clone(),
    }
}

fn device_to_dto(device: DeviceInfo) -> DeviceInfoDto {
    DeviceInfoDto {
        device_id: device.identity.device_id,
        user_id: device.identity.user_id,
        app_id: device.identity.app_id,
        linked: device.paired,
        address: device.address.map(|addr| addr.to_string()),
        last_seen_unix_secs: device
            .last_seen
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs()),
        fingerprint: device.fingerprint,
    }
}

fn update_config_device(state: &FranklyState, device: &DeviceInfo) -> Result<(), String> {
    let mut config = state
        .config
        .lock()
        .map_err(|_| "config lock poisoned".to_string())?;
    config.upsert_device(&device.identity, device.address, device.fingerprint.clone());
    save_config(&state.config_path, &config)
}

fn load_todos(path: &Path) -> Result<Vec<Todo>, String> {
    let conn = open_db(path)?;
    let mut stmt = conn
        .prepare("SELECT id, title, completed FROM todos ORDER BY rowid")
        .map_err(|error| error.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let completed: i64 = row.get(2)?;
            Ok(Todo {
                id: row.get(0)?,
                title: row.get(1)?,
                completed: completed != 0,
            })
        })
        .map_err(|error| error.to_string())?;
    let mut todos = Vec::new();
    for row in rows {
        todos.push(row.map_err(|error| error.to_string())?);
    }
    Ok(todos)
}

fn open_db(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let conn = Connection::open(path).map_err(|error| error.to_string())?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS todos (\n            id TEXT PRIMARY KEY,\n            title TEXT NOT NULL,\n            completed INTEGER NOT NULL\n        )",
        [],
    )
    .map_err(|error| error.to_string())?;
    Ok(conn)
}

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

struct AppPaths {
    config: PathBuf,
    state: PathBuf,
    db: PathBuf,
}

fn app_paths(app: &AppHandle) -> Result<AppPaths, String> {
    let base = app
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    fs::create_dir_all(&base).map_err(|error| error.to_string())?;
    Ok(AppPaths {
        config: base.join("config.json"),
        state: base.join("state.json"),
        db: base.join("todos.sqlite"),
    })
}

fn load_or_init_config(path: &Path) -> Result<(FranklyConfig, DeviceKeys), String> {
    if path.exists() {
        let data = fs::read(path).map_err(|error| error.to_string())?;
        let mut config: FranklyConfig =
            serde_json::from_slice(&data).map_err(|error| error.to_string())?;
        let keys = config.device_keys()?;
        if config.app_key.is_none() {
            let app_key = AppKey::generate().map_err(|error| error.to_string())?;
            config.set_app_key(&app_key);
            save_config(path, &config)?;
        }
        return Ok((config, keys));
    }

    let user_id = whoami::username();
    let device_id = format!("frankly-{}", Uuid::new_v4().simple());
    let identity = Identity::new(&device_id, APP_ID, &user_id);
    let keys = DeviceKeys::generate(&identity).map_err(|error| error.to_string())?;
    let app_key = AppKey::generate().map_err(|error| error.to_string())?;

    let config = FranklyConfig {
        app_id: APP_ID.to_string(),
        device_id,
        user_id,
        device_keys: DeviceKeysRecord::from_keys(&keys),
        app_key: Some(BASE64.encode(app_key.as_bytes())),
        devices: BTreeMap::new(),
    };

    save_config(path, &config)?;
    Ok((config, keys))
}

fn save_config(path: &Path, config: &FranklyConfig) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(config).map_err(|error| error.to_string())?;
    fs::write(path, data).map_err(|error| error.to_string())?;
    Ok(())
}

fn load_or_init_state(path: &Path, device_id: &str, app_key: &AppKey) -> Result<State, String> {
    if path.exists() {
        let state = State::load_maybe_encrypted(app_key, path).map_err(|error| error.to_string())?;
        state
            .save_encrypted(app_key, path)
            .map_err(|error| error.to_string())?;
        return Ok(state);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let state = State::new(device_id);
    state
        .save_encrypted(app_key, path)
        .map_err(|error| error.to_string())?;
    Ok(state)
}

fn init_state(app: &AppHandle) -> Result<FranklyState, String> {
    let paths = app_paths(app)?;
    let (config, keys) = load_or_init_config(&paths.config)?;
    let config = Arc::new(Mutex::new(config));
    let identity = config
        .lock()
        .map_err(|_| "config lock poisoned".to_string())?
        .identity();
    let app_key = config
        .lock()
        .map_err(|_| "config lock poisoned".to_string())?
        .app_key()?;

    let handler = Arc::new(FranklyHandler::new(
        identity.app_id.clone(),
        keys,
        Arc::clone(&config),
        paths.config.clone(),
    ));

    let state = load_or_init_state(&paths.state, &identity.device_id, &app_key)?;
    let listen_addr = LISTEN_ADDR
        .parse::<SocketAddr>()
        .map_err(|_| "invalid listen address".to_string())?;

    let mut engine = Engine::new(
        EngineConfig::new(identity.clone()).with_listen_addr(listen_addr),
        state,
        handler,
    );

    let adapter = Arc::new(SqliteFileAdapter::new(TODOS_KEY, &paths.db));
    engine
        .register_adapter(adapter)
        .map_err(|error| error.to_string())?;

    let _ = open_db(&paths.db)?;

    let listener_addr = engine
        .start_listening()
        .map_err(|error| error.to_string())?;
    let mdns = register_mdns(&identity, listener_addr).ok();
    let event_stream = engine.attach_event_channel();
    let auto_refresh = engine
        .auto_refresh(TODOS_KEY, &paths.state)
        .map_err(|error| error.to_string())?;
    spawn_event_forwarder(app.clone(), event_stream);

    Ok(FranklyState {
        engine: Mutex::new(engine),
        config,
        config_path: paths.config,
        state_path: paths.state,
        db_path: paths.db,
        listener_addr: Mutex::new(Some(listener_addr)),
        mdns: Mutex::new(mdns),
        auto_refresh: Mutex::new(Some(auto_refresh)),
    })
}

fn spawn_event_forwarder(app: AppHandle, stream: EventStream) {
    std::thread::spawn(move || {
        loop {
            let event = match stream.recv() {
                Ok(event) => event,
                Err(_) => break,
            };
            let payload = match event {
                Event::SyncStarted { device, adapter_id } => SyncEventDto {
                    kind: "started".to_string(),
                    adapter_id: Some(adapter_id),
                    device_id: Some(device.identity.device_id),
                    user_id: Some(device.identity.user_id),
                    result: None,
                    message: None,
                },
                Event::SyncFinished {
                    device,
                    adapter_id,
                    result,
                } => SyncEventDto {
                    kind: "finished".to_string(),
                    adapter_id: Some(adapter_id),
                    device_id: Some(device.identity.device_id),
                    user_id: Some(device.identity.user_id),
                    result: Some(match result {
                        libresync::SyncResult::Success => "success".to_string(),
                        libresync::SyncResult::Failed(reason) => reason,
                    }),
                    message: None,
                },
                Event::Error { message } => SyncEventDto {
                    kind: "error".to_string(),
                    adapter_id: None,
                    device_id: None,
                    user_id: None,
                    result: None,
                    message: Some(message),
                },
                _ => continue,
            };

            let _ = app.emit_all("sync_event", payload);
        }
    });
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let state = init_state(&app.handle())?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_status,
            list_devices,
            discover_devices,
            link_device,
            sync_device,
            revoke_device,
            get_todos,
            add_todo,
            update_todo,
            delete_todo,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Frankly");
}
