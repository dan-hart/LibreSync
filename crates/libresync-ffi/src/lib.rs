use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::net::SocketAddr;
use std::os::raw::{c_char, c_int, c_uchar};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use libresync::{
    event_channel, AppKey, BackupManager, CancelToken, DataAdapterBackup, DeviceHandler,
    DeviceInfo, DeviceKeys, Engine, EngineConfig, Event, EventSink, EventStream,
    FileLogicalAdapter, FileSnapshotStore, Identity, JsonFileAdapter, MergePolicy,
    RetentionPolicy, SqliteFileAdapter, SqliteLogicalAdapter, SqliteLogicalEncoding,
    SqliteLogicalField, SqliteLogicalMapping, State, SyncRequest, SyncResult,
};
use serde::{Deserialize, Serialize};

/// ABI version. 2 adds the event queue (`libresync_engine_event_*`),
/// asynchronous sync with tickets and cancellation, and the `fingerprint`
/// field on device JSON.
const ABI_VERSION: u32 = 2;

#[derive(Clone, Debug, Deserialize)]
struct FfiAllowlistEntry {
    device_id: String,
    fingerprint: String,
}

#[derive(Clone, Debug, Deserialize)]
struct FfiConfig {
    device_id: String,
    app_id: String,
    user_id: String,
    listen_addr: Option<String>,
    app_key: String,
    device_cert_der: String,
    device_key_der: String,
    #[serde(default)]
    allowlist: Vec<FfiAllowlistEntry>,
    #[serde(default)]
    auto_accept: bool,
    #[serde(default)]
    pairing_secret: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct FfiDeviceKeys {
    device_cert_der: String,
    device_key_der: String,
    fingerprint: String,
}

#[derive(Clone, Debug, Deserialize)]
struct FfiSqliteLogicalField {
    column: String,
    field: String,
    #[serde(default)]
    encoding: Option<SqliteLogicalEncoding>,
    #[serde(default)]
    merge_policy: Option<MergePolicy>,
}

#[derive(Clone, Debug, Deserialize)]
struct FfiSqliteLogicalMapping {
    data_table: String,
    id_column: String,
    schema: String,
    entity: String,
    fields: Vec<FfiSqliteLogicalField>,
    #[serde(default)]
    meta_table: Option<String>,
    #[serde(default)]
    default_merge_policy: Option<MergePolicy>,
}

impl FfiSqliteLogicalMapping {
    fn to_mapping(self) -> SqliteLogicalMapping {
        let mut mapping =
            SqliteLogicalMapping::new(self.data_table, self.id_column, self.schema, self.entity);
        if let Some(table) = self.meta_table {
            mapping = mapping.with_meta_table(table);
        }
        if let Some(policy) = self.default_merge_policy {
            mapping = mapping.with_default_merge_policy(policy);
        }
        for field in self.fields {
            let mut mapped = SqliteLogicalField::new(field.column, field.field);
            if let Some(encoding) = field.encoding {
                mapped = mapped.with_encoding(encoding);
            }
            if let Some(policy) = field.merge_policy {
                mapped = mapped.with_merge_policy(policy);
            }
            mapping = mapping.with_field_def(mapped);
        }
        mapping
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum FfiSqliteLogicalMappingInput {
    One(FfiSqliteLogicalMapping),
    Many(Vec<FfiSqliteLogicalMapping>),
}

impl FfiSqliteLogicalMappingInput {
    fn into_mappings(self) -> Vec<SqliteLogicalMapping> {
        match self {
            FfiSqliteLogicalMappingInput::One(mapping) => vec![mapping.to_mapping()],
            FfiSqliteLogicalMappingInput::Many(mappings) => mappings
                .into_iter()
                .map(FfiSqliteLogicalMapping::to_mapping)
                .collect(),
        }
    }
}

#[derive(Debug)]
struct FfiHandler {
    app_id: String,
    app_key: Mutex<AppKey>,
    device_keys: DeviceKeys,
    allowlist: Mutex<HashMap<String, String>>,
    auto_accept: AtomicBool,
    pairing_secret: Option<String>,
}

impl DeviceHandler for FfiHandler {
    fn app_id(&self) -> &str {
        &self.app_id
    }

    fn is_linked(&self, identity: &Identity) -> bool {
        self.allowlist
            .lock()
            .ok()
            .map(|map| map.contains_key(&identity.device_id))
            .unwrap_or(false)
    }

    fn approve_link(&self, _identity: &Identity) -> libresync::Result<bool> {
        Ok(self.auto_accept.load(Ordering::SeqCst))
    }

    fn app_key(&self) -> libresync::Result<AppKey> {
        self.app_key
            .lock()
            .map(|key| key.clone())
            .map_err(|_| libresync::Error::Protocol("app key lock poisoned".to_string()))
    }

    fn set_app_key(&self, app_key: &AppKey) -> libresync::Result<()> {
        let mut guard = self
            .app_key
            .lock()
            .map_err(|_| libresync::Error::Protocol("app key lock poisoned".to_string()))?;
        *guard = app_key.clone();
        Ok(())
    }

    fn device_keys(&self) -> libresync::Result<DeviceKeys> {
        Ok(self.device_keys.clone())
    }

    fn is_linked_with_fingerprint(&self, identity: &Identity, fingerprint: &str) -> bool {
        self.allowlist
            .lock()
            .ok()
            .and_then(|map| map.get(&identity.device_id).cloned())
            .map(|stored| stored == fingerprint)
            .unwrap_or(false)
    }

    fn approve_link_with_fingerprint(
        &self,
        _identity: &Identity,
        _fingerprint: &str,
    ) -> libresync::Result<bool> {
        Ok(self.auto_accept.load(Ordering::SeqCst))
    }

    fn pairing_secret(&self) -> Option<String> {
        self.pairing_secret.clone()
    }
}

/// Opaque engine handle. Cheap to share: background tasks hold their own
/// reference, so `libresync_engine_free` while a task runs is safe (the task
/// finishes and its events are dropped).
pub struct EngineHandle {
    inner: Arc<EngineInner>,
}

pub struct EngineInner {
    engine: Mutex<Engine>,
    handler: Arc<FfiHandler>,
    state_path: PathBuf,
    events: EventStream,
    sink: Arc<dyn EventSink>,
    tasks: Mutex<HashMap<u64, CancelToken>>,
    next_ticket: AtomicU64,
}

impl std::ops::Deref for EngineHandle {
    type Target = EngineInner;

    fn deref(&self) -> &EngineInner {
        &self.inner
    }
}

#[derive(Serialize)]
struct FfiDeviceInfo {
    device_id: String,
    user_id: String,
    app_id: String,
    address: Option<String>,
    linked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    fingerprint: Option<String>,
}

impl From<&DeviceInfo> for FfiDeviceInfo {
    fn from(device: &DeviceInfo) -> Self {
        Self {
            device_id: device.identity.device_id.clone(),
            user_id: device.identity.user_id.clone(),
            app_id: device.identity.app_id.clone(),
            address: device.address.map(|addr| addr.to_string()),
            linked: device.linked,
            fingerprint: device.fingerprint.clone(),
        }
    }
}

fn sync_result_json(result: &SyncResult) -> serde_json::Value {
    match result {
        SyncResult::Success => serde_json::json!({ "ok": true }),
        SyncResult::Failed(message) => serde_json::json!({ "ok": false, "error": message }),
    }
}

/// JSON encoding of an engine event: an object with a snake_case `type` and
/// the variant's fields. Unknown future variants encode as
/// `{"type":"unknown","debug":"..."}`.
fn event_to_json(event: &Event) -> serde_json::Value {
    match event {
        Event::LinkingRequested { request } => serde_json::json!({
            "type": "linking_requested",
            "device": FfiDeviceInfo::from(&request.device),
            "code": request.code,
        }),
        Event::LinkingDecisionRequired { request } => serde_json::json!({
            "type": "linking_decision_required",
            "device": FfiDeviceInfo::from(&request.device),
            "code": request.code,
        }),
        Event::SyncStarted { device, adapter_id } => serde_json::json!({
            "type": "sync_started",
            "device": FfiDeviceInfo::from(device),
            "adapter_id": adapter_id,
        }),
        Event::SyncFinished {
            device,
            adapter_id,
            result,
        } => serde_json::json!({
            "type": "sync_finished",
            "device": FfiDeviceInfo::from(device),
            "adapter_id": adapter_id,
            "result": sync_result_json(result),
        }),
        Event::DeviceSeen { device } => serde_json::json!({
            "type": "device_seen",
            "device": FfiDeviceInfo::from(device),
        }),
        Event::DeviceOffline { device } => serde_json::json!({
            "type": "device_offline",
            "device": FfiDeviceInfo::from(device),
        }),
        Event::Error { message } => serde_json::json!({
            "type": "error",
            "message": message,
        }),
        Event::FingerprintChanged {
            device,
            fingerprint,
        } => serde_json::json!({
            "type": "fingerprint_changed",
            "device": FfiDeviceInfo::from(device),
            "fingerprint": fingerprint,
        }),
        Event::InboundSync { device, applied } => serde_json::json!({
            "type": "inbound_sync",
            "device": FfiDeviceInfo::from(device),
            "applied": applied,
        }),
        Event::SyncStats {
            device,
            adapter_id,
            stats,
        } => serde_json::json!({
            "type": "sync_stats",
            "device": FfiDeviceInfo::from(device),
            "adapter_id": adapter_id,
            "entries_sent": stats.entries_sent,
            "entries_received": stats.entries_received,
            "entries_applied": stats.entries_applied,
            "bytes_sent": stats.bytes_sent,
            "bytes_received": stats.bytes_received,
            "full_snapshot": stats.full_snapshot,
            "protocol_version": stats.protocol_version,
        }),
        Event::LinkFinished {
            address,
            device,
            error,
        } => serde_json::json!({
            "type": "link_finished",
            "address": address.to_string(),
            "device": device.as_ref().map(FfiDeviceInfo::from),
            "error": error,
        }),
        Event::DiscoveryFinished { devices } => serde_json::json!({
            "type": "discovery_finished",
            "devices": devices.iter().map(FfiDeviceInfo::from).collect::<Vec<_>>(),
        }),
        Event::ListenerStarted { address } => serde_json::json!({
            "type": "listener_started",
            "address": address.to_string(),
        }),
        Event::ListenerStopped => serde_json::json!({ "type": "listener_stopped" }),
        Event::TaskFinished { ticket, result } => serde_json::json!({
            "type": "task_finished",
            "ticket": ticket,
            "result": sync_result_json(result),
        }),
        #[allow(unreachable_patterns)]
        other => serde_json::json!({
            "type": "unknown",
            "debug": format!("{other:?}"),
        }),
    }
}

#[derive(Serialize)]
struct FfiDiffCounts {
    new_entries: usize,
    changed_entries: usize,
    unchanged_entries: usize,
    missing_entries: usize,
}

#[derive(Serialize)]
struct FfiSnapshotSummary {
    total_snapshot_entries: usize,
    total_state_entries: usize,
    overall: FfiDiffCounts,
    by_group: HashMap<String, FfiDiffCounts>,
}

fn diff_counts_to_ffi(counts: libresync::DiffCounts) -> FfiDiffCounts {
    FfiDiffCounts {
        new_entries: counts.new_entries,
        changed_entries: counts.changed_entries,
        unchanged_entries: counts.unchanged_entries,
        missing_entries: counts.missing_entries,
    }
}

static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

fn set_last_error(message: impl Into<String>) {
    if let Ok(mut guard) = LAST_ERROR.lock() {
        *guard = Some(message.into());
    }
}

fn clear_last_error() {
    if let Ok(mut guard) = LAST_ERROR.lock() {
        *guard = None;
    }
}

fn cstr_to_string(ptr: *const c_char) -> Result<String, String> {
    if ptr.is_null() {
        return Err("null pointer".to_string());
    }
    let cstr = unsafe { CStr::from_ptr(ptr) };
    cstr.to_str()
        .map(|value| value.to_string())
        .map_err(|error| error.to_string())
}

fn build_engine(config: FfiConfig, state_path: PathBuf) -> Result<EngineHandle, String> {
    let app_key_bytes = BASE64
        .decode(config.app_key.as_bytes())
        .map_err(|error| error.to_string())?;
    let app_key = AppKey::from_slice(&app_key_bytes).map_err(|error| error.to_string())?;

    let cert_der = BASE64
        .decode(config.device_cert_der.as_bytes())
        .map_err(|error| error.to_string())?;
    let key_der = BASE64
        .decode(config.device_key_der.as_bytes())
        .map_err(|error| error.to_string())?;
    let device_keys = DeviceKeys::from_der(cert_der, key_der).map_err(|error| error.to_string())?;

    let allowlist = config
        .allowlist
        .into_iter()
        .map(|entry| (entry.device_id, entry.fingerprint))
        .collect::<HashMap<_, _>>();

    let pairing_secret = config.pairing_secret.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });

    let handler = Arc::new(FfiHandler {
        app_id: config.app_id.clone(),
        app_key: Mutex::new(app_key.clone()),
        device_keys,
        allowlist: Mutex::new(allowlist),
        auto_accept: AtomicBool::new(config.auto_accept),
        pairing_secret,
    });

    let identity = Identity::new(&config.device_id, &config.app_id, &config.user_id);
    let mut engine_config = EngineConfig::new(identity);
    if let Some(listen_addr) = config.listen_addr {
        let addr = listen_addr
            .parse::<SocketAddr>()
            .map_err(|error| error.to_string())?;
        engine_config = engine_config.with_listen_addr(addr);
    }

    let state = if state_path.exists() {
        State::load_maybe_encrypted(&app_key, &state_path)
            .unwrap_or_else(|_| State::new(config.device_id.clone()))
    } else {
        State::new(config.device_id.clone())
    };

    let mut engine = Engine::new(engine_config, state, handler.clone());
    let (sink, events) = event_channel();
    engine.set_event_sink(Arc::clone(&sink));
    Ok(EngineHandle {
        inner: Arc::new(EngineInner {
            engine: Mutex::new(engine),
            handler,
            state_path,
            events,
            sink,
            tasks: Mutex::new(HashMap::new()),
            next_ticket: AtomicU64::new(1),
        }),
    })
}

fn with_engine<F>(handle: *mut EngineHandle, f: F) -> bool
where
    F: FnOnce(&EngineHandle) -> Result<(), String>,
{
    clear_last_error();
    if handle.is_null() {
        set_last_error("engine handle is null");
        return false;
    }
    let handle = unsafe { &*handle };
    if let Err(error) = f(handle) {
        set_last_error(error);
        return false;
    }
    true
}

fn handle_ref<'a>(handle: *mut EngineHandle) -> Option<&'a EngineHandle> {
    clear_last_error();
    if handle.is_null() {
        set_last_error("engine handle is null");
        return None;
    }
    Some(unsafe { &*handle })
}

/// Runs one sync on the engine, then records the peer fingerprint and saves
/// the state. Shared by the blocking and asynchronous entry points.
fn run_sync(inner: &EngineInner, request: &SyncRequest) -> Result<(), String> {
    let engine = inner.engine.lock().map_err(|_| "engine lock".to_string())?;
    let (device, _) = engine.sync(request).map_err(|error| error.to_string())?;
    if let Some(fingerprint) = device.fingerprint.clone() {
        let mut map = inner
            .handler
            .allowlist
            .lock()
            .map_err(|_| "allowlist lock".to_string())?;
        map.insert(device.identity.device_id, fingerprint);
    }
    let app_key = inner
        .handler
        .app_key()
        .map_err(|error| error.to_string())?;
    let state = engine.state();
    state
        .lock()
        .map_err(|_| "state lock".to_string())?
        .save_encrypted(&app_key, &inner.state_path)
        .map_err(|error| error.to_string())?;
    Ok(())
}

#[no_mangle]
pub extern "C" fn libresync_abi_version() -> u32 {
    ABI_VERSION
}

#[no_mangle]
pub extern "C" fn libresync_generate_app_key() -> *mut c_char {
    clear_last_error();
    let key = match AppKey::generate() {
        Ok(key) => key,
        Err(error) => {
            set_last_error(error.to_string());
            return std::ptr::null_mut();
        }
    };
    let encoded = BASE64.encode(key.as_bytes());
    CString::new(encoded)
        .map(|value| value.into_raw())
        .unwrap_or_else(|_| std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn libresync_generate_device_keys(
    device_id: *const c_char,
    app_id: *const c_char,
    user_id: *const c_char,
) -> *mut c_char {
    clear_last_error();
    let device_id = match cstr_to_string(device_id) {
        Ok(value) => value,
        Err(error) => {
            set_last_error(error);
            return std::ptr::null_mut();
        }
    };
    let app_id = match cstr_to_string(app_id) {
        Ok(value) => value,
        Err(error) => {
            set_last_error(error);
            return std::ptr::null_mut();
        }
    };
    let user_id = match cstr_to_string(user_id) {
        Ok(value) => value,
        Err(error) => {
            set_last_error(error);
            return std::ptr::null_mut();
        }
    };
    let identity = Identity::new(&device_id, &app_id, &user_id);
    let keys = match DeviceKeys::generate(&identity) {
        Ok(keys) => keys,
        Err(error) => {
            set_last_error(error.to_string());
            return std::ptr::null_mut();
        }
    };
    let payload = FfiDeviceKeys {
        device_cert_der: BASE64.encode(keys.cert_der()),
        device_key_der: BASE64.encode(keys.key_der()),
        fingerprint: keys.fingerprint().to_string(),
    };
    let json = match serde_json::to_string(&payload) {
        Ok(json) => json,
        Err(error) => {
            set_last_error(error.to_string());
            return std::ptr::null_mut();
        }
    };
    CString::new(json)
        .map(|value| value.into_raw())
        .unwrap_or_else(|_| std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn libresync_last_error() -> *mut c_char {
    if let Ok(mut guard) = LAST_ERROR.lock() {
        if let Some(message) = guard.take() {
            if let Ok(cstr) = CString::new(message) {
                return cstr.into_raw();
            }
        }
    }
    std::ptr::null_mut()
}

#[no_mangle]
pub extern "C" fn libresync_string_free(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        let _ = CString::from_raw(ptr);
    }
}

#[no_mangle]
pub extern "C" fn libresync_engine_create(
    config_json: *const c_char,
    state_path: *const c_char,
) -> *mut EngineHandle {
    clear_last_error();
    let config_json = match cstr_to_string(config_json) {
        Ok(value) => value,
        Err(error) => {
            set_last_error(error);
            return std::ptr::null_mut();
        }
    };
    let state_path = match cstr_to_string(state_path) {
        Ok(value) => value,
        Err(error) => {
            set_last_error(error);
            return std::ptr::null_mut();
        }
    };

    let config: FfiConfig = match serde_json::from_str(&config_json) {
        Ok(value) => value,
        Err(error) => {
            set_last_error(error.to_string());
            return std::ptr::null_mut();
        }
    };

    match build_engine(config, PathBuf::from(state_path)) {
        Ok(handle) => Box::into_raw(Box::new(handle)),
        Err(error) => {
            set_last_error(error);
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn libresync_engine_free(handle: *mut EngineHandle) {
    if handle.is_null() {
        return;
    }
    unsafe {
        let _ = Box::from_raw(handle);
    }
}

#[no_mangle]
pub extern "C" fn libresync_engine_set_auto_accept(
    handle: *mut EngineHandle,
    enabled: bool,
) -> bool {
    with_engine(handle, |handle| {
        handle.handler.auto_accept.store(enabled, Ordering::SeqCst);
        Ok(())
    })
}

#[no_mangle]
pub extern "C" fn libresync_engine_register_json_adapter(
    handle: *mut EngineHandle,
    adapter_id: *const c_char,
    path: *const c_char,
) -> bool {
    with_engine(handle, |handle| {
        let adapter_id = cstr_to_string(adapter_id)?;
        let path = cstr_to_string(path)?;
        let adapter = JsonFileAdapter::new(adapter_id, path);
        let mut engine = handle
            .engine
            .lock()
            .map_err(|_| "engine lock".to_string())?;
        engine
            .register_adapter(Arc::new(adapter))
            .map_err(|error| error.to_string())?;
        Ok(())
    })
}

#[no_mangle]
pub extern "C" fn libresync_engine_register_logical_file_adapter(
    handle: *mut EngineHandle,
    adapter_id: *const c_char,
    namespace: *const c_char,
    path: *const c_char,
) -> bool {
    with_engine(handle, |handle| {
        let adapter_id = cstr_to_string(adapter_id)?;
        let namespace = cstr_to_string(namespace)?;
        let path = cstr_to_string(path)?;
        let adapter = FileLogicalAdapter::new(adapter_id, namespace, path);
        let mut engine = handle
            .engine
            .lock()
            .map_err(|_| "engine lock".to_string())?;
        engine
            .register_logical_adapter(Arc::new(adapter))
            .map_err(|error| error.to_string())?;
        Ok(())
    })
}

#[no_mangle]
pub extern "C" fn libresync_engine_register_sqlite_adapter(
    handle: *mut EngineHandle,
    adapter_id: *const c_char,
    path: *const c_char,
    page_delta: usize,
) -> bool {
    with_engine(handle, |handle| {
        let adapter_id = cstr_to_string(adapter_id)?;
        let path = cstr_to_string(path)?;
        let adapter = if page_delta > 0 {
            SqliteFileAdapter::new(adapter_id, path).with_page_delta(page_delta)
        } else {
            SqliteFileAdapter::new(adapter_id, path)
        };
        let mut engine = handle
            .engine
            .lock()
            .map_err(|_| "engine lock".to_string())?;
        engine
            .register_adapter(Arc::new(adapter))
            .map_err(|error| error.to_string())?;
        Ok(())
    })
}

#[no_mangle]
pub extern "C" fn libresync_engine_register_sqlite_logical_adapter(
    handle: *mut EngineHandle,
    adapter_id: *const c_char,
    namespace: *const c_char,
    path: *const c_char,
    mapping_json: *const c_char,
) -> bool {
    with_engine(handle, |handle| {
        let adapter_id = cstr_to_string(adapter_id)?;
        let namespace = cstr_to_string(namespace)?;
        let path = cstr_to_string(path)?;
        let mapping_json = cstr_to_string(mapping_json)?;
        let mapping_input: FfiSqliteLogicalMappingInput = serde_json::from_str(&mapping_json)
            .map_err(|error| format!("invalid mapping JSON: {error}"))?;
        let mappings = mapping_input.into_mappings();
        if mappings.is_empty() {
            return Err("invalid mapping JSON: at least one mapping is required".to_string());
        }

        let mut adapter = SqliteLogicalAdapter::new(adapter_id, namespace, path, "records");
        for mapping in mappings {
            adapter = adapter.with_mapping(mapping);
        }
        let mut engine = handle
            .engine
            .lock()
            .map_err(|_| "engine lock".to_string())?;
        engine
            .register_logical_adapter(Arc::new(adapter))
            .map_err(|error| error.to_string())?;
        Ok(())
    })
}

#[no_mangle]
pub extern "C" fn libresync_engine_start_listening(handle: *mut EngineHandle) -> bool {
    with_engine(handle, |handle| {
        let mut engine = handle
            .engine
            .lock()
            .map_err(|_| "engine lock".to_string())?;
        engine
            .start_listening()
            .map_err(|error| error.to_string())?;
        Ok(())
    })
}

#[no_mangle]
pub extern "C" fn libresync_engine_stop_listening(handle: *mut EngineHandle) -> bool {
    with_engine(handle, |handle| {
        let mut engine = handle
            .engine
            .lock()
            .map_err(|_| "engine lock".to_string())?;
        engine.stop_listening().map_err(|error| error.to_string())?;
        Ok(())
    })
}

#[no_mangle]
pub extern "C" fn libresync_engine_discover(
    handle: *mut EngineHandle,
    timeout_ms: u64,
) -> *mut c_char {
    clear_last_error();
    if handle.is_null() {
        set_last_error("engine handle is null");
        return std::ptr::null_mut();
    }
    let handle = unsafe { &mut *handle };
    let engine = match handle.engine.lock() {
        Ok(engine) => engine,
        Err(_) => {
            set_last_error("engine lock".to_string());
            return std::ptr::null_mut();
        }
    };
    let devices =
        match engine.discover_devices_with_timeout(std::time::Duration::from_millis(timeout_ms)) {
            Ok(devices) => devices,
            Err(error) => {
                set_last_error(error.to_string());
                return std::ptr::null_mut();
            }
        };
    let info = devices
        .iter()
        .map(FfiDeviceInfo::from)
        .collect::<Vec<_>>();
    let json = match serde_json::to_string(&info) {
        Ok(json) => json,
        Err(error) => {
            set_last_error(error.to_string());
            return std::ptr::null_mut();
        }
    };
    CString::new(json)
        .map(|cstr| cstr.into_raw())
        .unwrap_or_else(|_| std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn libresync_allowlist_add(
    handle: *mut EngineHandle,
    device_id: *const c_char,
    fingerprint: *const c_char,
) -> bool {
    with_engine(handle, |handle| {
        let device_id = cstr_to_string(device_id)?;
        let fingerprint = cstr_to_string(fingerprint)?;
        let mut map = handle
            .handler
            .allowlist
            .lock()
            .map_err(|_| "allowlist lock".to_string())?;
        map.insert(device_id, fingerprint);
        Ok(())
    })
}

#[no_mangle]
pub extern "C" fn libresync_allowlist_clear(handle: *mut EngineHandle) -> bool {
    with_engine(handle, |handle| {
        let mut map = handle
            .handler
            .allowlist
            .lock()
            .map_err(|_| "allowlist lock".to_string())?;
        map.clear();
        Ok(())
    })
}

#[no_mangle]
pub extern "C" fn libresync_engine_link(
    handle: *mut EngineHandle,
    address: *const c_char,
) -> *mut c_char {
    clear_last_error();
    if handle.is_null() {
        set_last_error("engine handle is null");
        return std::ptr::null_mut();
    }
    let handle = unsafe { &mut *handle };
    let address = match cstr_to_string(address) {
        Ok(value) => value,
        Err(error) => {
            set_last_error(error);
            return std::ptr::null_mut();
        }
    };
    let addr = match address.parse::<SocketAddr>() {
        Ok(addr) => addr,
        Err(error) => {
            set_last_error(error.to_string());
            return std::ptr::null_mut();
        }
    };
    let engine = match handle.engine.lock() {
        Ok(engine) => engine,
        Err(_) => {
            set_last_error("engine lock".to_string());
            return std::ptr::null_mut();
        }
    };
    let device = match engine.request_link(addr) {
        Ok(device) => device,
        Err(error) => {
            set_last_error(error.to_string());
            return std::ptr::null_mut();
        }
    };
    if let Some(fingerprint) = device.fingerprint.clone() {
        if let Ok(mut map) = handle.handler.allowlist.lock() {
            map.insert(device.identity.device_id.clone(), fingerprint);
        }
    }
    let info = FfiDeviceInfo::from(&device);
    let json = match serde_json::to_string(&info) {
        Ok(json) => json,
        Err(error) => {
            set_last_error(error.to_string());
            return std::ptr::null_mut();
        }
    };
    CString::new(json)
        .map(|cstr| cstr.into_raw())
        .unwrap_or_else(|_| std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn libresync_engine_sync_now(
    handle: *mut EngineHandle,
    address: *const c_char,
    adapter_id: *const c_char,
) -> bool {
    with_engine(handle, |handle| {
        let address = cstr_to_string(address)?;
        let adapter_id = cstr_to_string(adapter_id)?;
        let addr = address
            .parse::<SocketAddr>()
            .map_err(|error| error.to_string())?;
        run_sync(handle, &SyncRequest::new(addr, adapter_id))
    })
}

/// Starts a sync on a background thread and returns a ticket (0 on error).
/// Progress arrives on the event queue: `sync_started`, `sync_finished`,
/// `sync_stats`, then `task_finished` with the ticket. Other engine calls
/// block while the sync holds the engine; the event queue never blocks.
#[no_mangle]
pub extern "C" fn libresync_engine_sync_async(
    handle: *mut EngineHandle,
    address: *const c_char,
    adapter_id: *const c_char,
) -> u64 {
    let handle = match handle_ref(handle) {
        Some(handle) => handle,
        None => return 0,
    };
    let parsed = (|| -> Result<(SocketAddr, String), String> {
        let address = cstr_to_string(address)?;
        let adapter_id = cstr_to_string(adapter_id)?;
        let addr = address
            .parse::<SocketAddr>()
            .map_err(|error| error.to_string())?;
        Ok((addr, adapter_id))
    })();
    let (addr, adapter_id) = match parsed {
        Ok(values) => values,
        Err(error) => {
            set_last_error(error);
            return 0;
        }
    };

    let ticket = handle.next_ticket.fetch_add(1, Ordering::SeqCst);
    let cancel = CancelToken::new();
    if let Ok(mut tasks) = handle.tasks.lock() {
        tasks.insert(ticket, cancel.clone());
    }
    let inner = Arc::clone(&handle.inner);
    std::thread::spawn(move || {
        let request = SyncRequest::new(addr, adapter_id).with_cancel(cancel);
        let result = run_sync(&inner, &request);
        if let Ok(mut tasks) = inner.tasks.lock() {
            tasks.remove(&ticket);
        }
        inner.sink.emit(Event::TaskFinished {
            ticket,
            result: match result {
                Ok(()) => SyncResult::Success,
                Err(error) => SyncResult::Failed(error),
            },
        });
    });
    ticket
}

/// Cancels a running asynchronous sync. Returns false when the ticket is
/// unknown or already finished.
#[no_mangle]
pub extern "C" fn libresync_engine_cancel(handle: *mut EngineHandle, ticket: u64) -> bool {
    let handle = match handle_ref(handle) {
        Some(handle) => handle,
        None => return false,
    };
    let cancel = handle
        .tasks
        .lock()
        .ok()
        .and_then(|tasks| tasks.get(&ticket).cloned());
    match cancel {
        Some(cancel) => {
            cancel.cancel();
            true
        }
        None => {
            set_last_error("unknown or finished ticket");
            false
        }
    }
}

/// Returns the next queued event as JSON, or NULL when the queue is empty
/// (`libresync_last_error` is then NULL too). Never blocks. Drain until NULL
/// after the event descriptor becomes readable.
#[no_mangle]
pub extern "C" fn libresync_engine_event_next(handle: *mut EngineHandle) -> *mut c_char {
    let handle = match handle_ref(handle) {
        Some(handle) => handle,
        None => return std::ptr::null_mut(),
    };
    match handle.events.try_recv() {
        Ok(Some(event)) => json_to_cstring(&event_to_json(&event)),
        Ok(None) => std::ptr::null_mut(),
        Err(error) => {
            set_last_error(error.to_string());
            std::ptr::null_mut()
        }
    }
}

/// Blocks up to `timeout_ms` for the next event; NULL on timeout. Meant for a
/// dedicated background thread (e.g. a DispatchQueue), never the UI thread.
#[no_mangle]
pub extern "C" fn libresync_engine_event_wait(
    handle: *mut EngineHandle,
    timeout_ms: u64,
) -> *mut c_char {
    let handle = match handle_ref(handle) {
        Some(handle) => handle,
        None => return std::ptr::null_mut(),
    };
    match handle
        .events
        .recv_timeout(Duration::from_millis(timeout_ms))
    {
        Ok(event) => json_to_cstring(&event_to_json(&event)),
        Err(_) => std::ptr::null_mut(),
    }
}

/// File descriptor that becomes readable while events are queued, for
/// `g_unix_fd_add` / `DispatchSource.makeReadSource`. Do not read from it;
/// call `libresync_engine_event_next` until NULL. Returns -1 when unavailable.
#[no_mangle]
pub extern "C" fn libresync_engine_event_fd(handle: *mut EngineHandle) -> c_int {
    match handle_ref(handle) {
        Some(handle) => handle.events.raw_fd().map(|fd| fd as c_int).unwrap_or(-1),
        None => -1,
    }
}

fn json_to_cstring(value: &serde_json::Value) -> *mut c_char {
    match CString::new(value.to_string()) {
        Ok(cstring) => cstring.into_raw(),
        Err(error) => {
            set_last_error(error.to_string());
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn libresync_engine_save_state(handle: *mut EngineHandle) -> bool {
    with_engine(handle, |handle| {
        let app_key = handle
            .handler
            .app_key()
            .map_err(|error| error.to_string())?;
        let engine = handle
            .engine
            .lock()
            .map_err(|_| "engine lock".to_string())?;
        let state = engine.state();
        state
            .lock()
            .map_err(|_| "state lock".to_string())?
            .save_encrypted(&app_key, &handle.state_path)
            .map_err(|error| error.to_string())?;
        Ok(())
    })
}

#[no_mangle]
pub extern "C" fn libresync_backup_prune(
    handle: *mut EngineHandle,
    adapter_id: *const c_char,
    max_snapshots: usize,
    max_age_days: u64,
) -> bool {
    with_engine(handle, |handle| {
        let adapter_id = cstr_to_string(adapter_id)?;
        let app_key = handle
            .handler
            .app_key()
            .map_err(|error| error.to_string())?;
        let backup_dir = handle
            .state_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("backups");
        let store = FileSnapshotStore::new(&backup_dir).map_err(|error| error.to_string())?;
        let manager = BackupManager::new(app_key, Arc::new(store));
        let policy = RetentionPolicy {
            max_snapshots: if max_snapshots == 0 {
                None
            } else {
                Some(max_snapshots)
            },
            max_age_secs: if max_age_days == 0 {
                None
            } else {
                Some(max_age_days.saturating_mul(24 * 60 * 60))
            },
        };
        manager
            .prune_snapshots(&adapter_id, policy)
            .map_err(|error| error.to_string())?;
        Ok(())
    })
}

#[no_mangle]
pub extern "C" fn libresync_backup_snapshot(
    handle: *mut EngineHandle,
    adapter_id: *const c_char,
    note: *const c_char,
) -> *mut c_char {
    clear_last_error();
    if handle.is_null() {
        set_last_error("engine handle is null");
        return std::ptr::null_mut();
    }
    let handle = unsafe { &mut *handle };
    let adapter_id = match cstr_to_string(adapter_id) {
        Ok(value) => value,
        Err(error) => {
            set_last_error(error);
            return std::ptr::null_mut();
        }
    };
    let note = if note.is_null() {
        None
    } else {
        Some(cstr_to_string(note).unwrap_or_default())
    };

    let app_key = match handle.handler.app_key() {
        Ok(key) => key,
        Err(error) => {
            set_last_error(error.to_string());
            return std::ptr::null_mut();
        }
    };
    let backup_dir = handle
        .state_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("backups");
    let store = match FileSnapshotStore::new(&backup_dir) {
        Ok(store) => store,
        Err(error) => {
            set_last_error(error.to_string());
            return std::ptr::null_mut();
        }
    };
    let manager = BackupManager::new(app_key, Arc::new(store));
    let engine = match handle.engine.lock() {
        Ok(engine) => engine,
        Err(_) => {
            set_last_error("engine lock".to_string());
            return std::ptr::null_mut();
        }
    };
    let adapter = match engine.adapter(&adapter_id) {
        Some(adapter) => adapter,
        None => {
            set_last_error("adapter not found".to_string());
            return std::ptr::null_mut();
        }
    };
    let adapter = DataAdapterBackup::new(adapter);
    let state = engine.state();
    let state = match state.lock() {
        Ok(state) => state,
        Err(_) => {
            set_last_error("state lock".to_string());
            return std::ptr::null_mut();
        }
    };
    let metadata = match manager.create_snapshot(&adapter, &state, note) {
        Ok(metadata) => metadata,
        Err(error) => {
            set_last_error(error.to_string());
            return std::ptr::null_mut();
        }
    };
    let json = match serde_json::to_string(&metadata) {
        Ok(json) => json,
        Err(error) => {
            set_last_error(error.to_string());
            return std::ptr::null_mut();
        }
    };
    CString::new(json)
        .map(|cstr| cstr.into_raw())
        .unwrap_or_else(|_| std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn libresync_backup_list(
    handle: *mut EngineHandle,
    adapter_id: *const c_char,
) -> *mut c_char {
    clear_last_error();
    if handle.is_null() {
        set_last_error("engine handle is null");
        return std::ptr::null_mut();
    }
    let handle = unsafe { &mut *handle };
    let adapter_id = match cstr_to_string(adapter_id) {
        Ok(value) => value,
        Err(error) => {
            set_last_error(error);
            return std::ptr::null_mut();
        }
    };

    let app_key = match handle.handler.app_key() {
        Ok(key) => key,
        Err(error) => {
            set_last_error(error.to_string());
            return std::ptr::null_mut();
        }
    };
    let backup_dir = handle
        .state_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("backups");
    let store = match FileSnapshotStore::new(&backup_dir) {
        Ok(store) => store,
        Err(error) => {
            set_last_error(error.to_string());
            return std::ptr::null_mut();
        }
    };
    let manager = BackupManager::new(app_key, Arc::new(store));
    let snapshots = match manager.list_snapshots(&adapter_id) {
        Ok(snapshots) => snapshots,
        Err(error) => {
            set_last_error(error.to_string());
            return std::ptr::null_mut();
        }
    };
    let json = match serde_json::to_string(&snapshots) {
        Ok(json) => json,
        Err(error) => {
            set_last_error(error.to_string());
            return std::ptr::null_mut();
        }
    };
    CString::new(json)
        .map(|cstr| cstr.into_raw())
        .unwrap_or_else(|_| std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn libresync_backup_preview(
    handle: *mut EngineHandle,
    adapter_id: *const c_char,
    snapshot_id: *const c_char,
) -> *mut c_char {
    clear_last_error();
    if handle.is_null() {
        set_last_error("engine handle is null");
        return std::ptr::null_mut();
    }
    let handle = unsafe { &mut *handle };
    let adapter_id = match cstr_to_string(adapter_id) {
        Ok(value) => value,
        Err(error) => {
            set_last_error(error);
            return std::ptr::null_mut();
        }
    };
    let snapshot_id = match cstr_to_string(snapshot_id) {
        Ok(value) => value,
        Err(error) => {
            set_last_error(error);
            return std::ptr::null_mut();
        }
    };

    let app_key = match handle.handler.app_key() {
        Ok(key) => key,
        Err(error) => {
            set_last_error(error.to_string());
            return std::ptr::null_mut();
        }
    };
    let backup_dir = handle
        .state_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("backups");
    let store = match FileSnapshotStore::new(&backup_dir) {
        Ok(store) => store,
        Err(error) => {
            set_last_error(error.to_string());
            return std::ptr::null_mut();
        }
    };
    let manager = BackupManager::new(app_key, Arc::new(store));
    let engine = match handle.engine.lock() {
        Ok(engine) => engine,
        Err(_) => {
            set_last_error("engine lock".to_string());
            return std::ptr::null_mut();
        }
    };
    let state_handle = engine.state();
    let state = match state_handle.lock() {
        Ok(state) => state,
        Err(_) => {
            set_last_error("state lock".to_string());
            return std::ptr::null_mut();
        }
    };
    let (_meta, entries) = match manager.load_snapshot_entries(&adapter_id, &snapshot_id) {
        Ok(result) => result,
        Err(error) => {
            set_last_error(error.to_string());
            return std::ptr::null_mut();
        }
    };
    let summary = match libresync::summarize_snapshot_diff(&state, &entries) {
        Ok(summary) => summary,
        Err(error) => {
            set_last_error(error.to_string());
            return std::ptr::null_mut();
        }
    };
    let ffi_summary = FfiSnapshotSummary {
        total_snapshot_entries: summary.total_snapshot_entries,
        total_state_entries: summary.total_state_entries,
        overall: diff_counts_to_ffi(summary.overall),
        by_group: summary
            .by_group
            .into_iter()
            .map(|(key, counts)| (key, diff_counts_to_ffi(counts)))
            .collect(),
    };
    let json = match serde_json::to_string(&ffi_summary) {
        Ok(json) => json,
        Err(error) => {
            set_last_error(error.to_string());
            return std::ptr::null_mut();
        }
    };
    CString::new(json)
        .map(|cstr| cstr.into_raw())
        .unwrap_or_else(|_| std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn libresync_backup_restore(
    handle: *mut EngineHandle,
    adapter_id: *const c_char,
    snapshot_id: *const c_char,
) -> bool {
    with_engine(handle, |handle| {
        let adapter_id = cstr_to_string(adapter_id)?;
        let snapshot_id = cstr_to_string(snapshot_id)?;
        let app_key = handle
            .handler
            .app_key()
            .map_err(|error| error.to_string())?;
        let backup_dir = handle
            .state_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("backups");
        let store = FileSnapshotStore::new(&backup_dir).map_err(|error| error.to_string())?;
        let manager = BackupManager::new(app_key, Arc::new(store));
        let engine = handle
            .engine
            .lock()
            .map_err(|_| "engine lock".to_string())?;
        let adapter = engine
            .adapter(&adapter_id)
            .ok_or("adapter not found".to_string())?;
        let adapter = DataAdapterBackup::new(adapter);
        let state = engine.state();
        let mut state = state.lock().map_err(|_| "state lock".to_string())?;
        manager
            .restore_snapshot(
                &adapter,
                &mut state,
                &snapshot_id,
                libresync::RestoreOptions::confirmed(),
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    })
}

#[no_mangle]
pub extern "C" fn libresync_bytes_free(ptr: *mut c_uchar, len: usize) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        let _ = Vec::from_raw_parts(ptr, len, len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use tempfile::TempDir;

    fn take_string(ptr: *mut c_char) -> Option<String> {
        if ptr.is_null() {
            return None;
        }
        let message = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().to_string();
        libresync_string_free(ptr);
        Some(message)
    }

    fn last_error() -> Option<String> {
        take_string(libresync_last_error())
    }

    fn build_config(
        listen_addr: Option<&str>,
        auto_accept: bool,
        pairing_secret: Option<&str>,
    ) -> (CString, CString, TempDir) {
        let tempdir = TempDir::new().expect("tempdir");
        let state_path = tempdir.path().join("state.json");
        let identity = Identity::new("device-1", "app-1", "user-1");
        let device_keys = DeviceKeys::generate(&identity).expect("device keys");
        let app_key = AppKey::generate().expect("app key");
        let mut config = serde_json::json!({
            "device_id": identity.device_id,
            "app_id": identity.app_id,
            "user_id": identity.user_id,
            "listen_addr": listen_addr,
            "app_key": BASE64.encode(app_key.as_bytes()),
            "device_cert_der": BASE64.encode(device_keys.cert_der()),
            "device_key_der": BASE64.encode(device_keys.key_der()),
            "allowlist": [],
            "auto_accept": auto_accept,
        });
        if let Some(secret) = pairing_secret {
            config["pairing_secret"] = serde_json::Value::String(secret.to_string());
        }
        let config_json = CString::new(config.to_string()).expect("config json");
        let state_cstr =
            CString::new(state_path.to_string_lossy().to_string()).expect("state path");
        (config_json, state_cstr, tempdir)
    }

    fn create_engine(listen_addr: Option<&str>) -> (TempDir, *mut EngineHandle) {
        let (config_json, state_path, tempdir) = build_config(listen_addr, true, None);
        let handle = libresync_engine_create(config_json.as_ptr(), state_path.as_ptr());
        assert!(!handle.is_null(), "engine should be created");
        (tempdir, handle)
    }

    fn create_engine_with_secret(secret: Option<&str>) -> (TempDir, *mut EngineHandle) {
        let (config_json, state_path, tempdir) = build_config(None, true, secret);
        let handle = libresync_engine_create(config_json.as_ptr(), state_path.as_ptr());
        assert!(!handle.is_null(), "engine should be created");
        (tempdir, handle)
    }

    #[test]
    fn ffi_abi_and_last_error_paths() {
        assert_eq!(libresync_abi_version(), ABI_VERSION);

        assert!(last_error().is_none());

        let handle = libresync_engine_create(std::ptr::null(), std::ptr::null());
        assert!(handle.is_null());
        let _ = last_error();

        let bad_config = CString::new("not json").expect("cstr");
        let state_path = CString::new("state.json").expect("state");
        let handle = libresync_engine_create(bad_config.as_ptr(), state_path.as_ptr());
        assert!(handle.is_null());
        assert!(last_error().is_some());

        let mut bytes = vec![1u8, 2u8, 3u8];
        let ptr = bytes.as_mut_ptr();
        let len = bytes.len();
        std::mem::forget(bytes);
        libresync_bytes_free(ptr, len);
    }

    #[test]
    fn ffi_listener_cycle() {
        let (_tempdir, handle) = create_engine(Some("127.0.0.1:0"));

        assert!(libresync_engine_start_listening(handle));
        assert!(libresync_engine_stop_listening(handle));

        assert!(!libresync_engine_stop_listening(handle));
        let err = last_error().unwrap_or_default();
        assert!(err.contains("listener not running"));

        libresync_engine_free(handle);
    }

    #[test]
    fn ffi_registers_adapters_and_allowlist() {
        let (tempdir, handle) = create_engine(None);
        let json_path = tempdir.path().join("data.json");
        let logical_path = tempdir.path().join("records.json");
        let sqlite_path = tempdir.path().join("data.sqlite");

        let adapter_id = CString::new("doc").expect("adapter id");
        let json_path_c = CString::new(json_path.to_string_lossy().to_string()).expect("json path");
        assert!(libresync_engine_register_json_adapter(
            handle,
            adapter_id.as_ptr(),
            json_path_c.as_ptr()
        ));

        let logical_id = CString::new("logical").expect("logical id");
        let namespace = CString::new("notes").expect("namespace");
        let logical_path_c =
            CString::new(logical_path.to_string_lossy().to_string()).expect("logical path");
        assert!(libresync_engine_register_logical_file_adapter(
            handle,
            logical_id.as_ptr(),
            namespace.as_ptr(),
            logical_path_c.as_ptr(),
        ));

        let sqlite_id = CString::new("db").expect("sqlite id");
        let sqlite_path_c =
            CString::new(sqlite_path.to_string_lossy().to_string()).expect("sqlite path");
        assert!(libresync_engine_register_sqlite_adapter(
            handle,
            sqlite_id.as_ptr(),
            sqlite_path_c.as_ptr(),
            512
        ));

        let sqlite_id_alt = CString::new("db2").expect("sqlite id");
        assert!(libresync_engine_register_sqlite_adapter(
            handle,
            sqlite_id_alt.as_ptr(),
            sqlite_path_c.as_ptr(),
            0
        ));

        assert!(libresync_engine_set_auto_accept(handle, false));

        assert!(!libresync_engine_register_json_adapter(
            handle,
            adapter_id.as_ptr(),
            json_path_c.as_ptr()
        ));
        assert!(last_error().is_some());

        let device_id = CString::new("device-2").expect("device id");
        let fingerprint = CString::new("abc123").expect("fingerprint");
        assert!(libresync_allowlist_add(
            handle,
            device_id.as_ptr(),
            fingerprint.as_ptr()
        ));
        assert!(libresync_allowlist_clear(handle));

        libresync_engine_free(handle);
    }

    #[test]
    fn ffi_backup_flow() {
        let (tempdir, handle) = create_engine(None);
        let json_path = tempdir.path().join("data.json");

        let adapter_id = CString::new("doc").expect("adapter id");
        let json_path_c = CString::new(json_path.to_string_lossy().to_string()).expect("json path");
        assert!(libresync_engine_register_json_adapter(
            handle,
            adapter_id.as_ptr(),
            json_path_c.as_ptr()
        ));

        assert!(libresync_engine_save_state(handle));

        let note = CString::new("initial").expect("note");
        let snapshot_ptr = libresync_backup_snapshot(handle, adapter_id.as_ptr(), note.as_ptr());
        let snapshot_json = take_string(snapshot_ptr).expect("snapshot json");
        let metadata: libresync::SnapshotMetadata =
            serde_json::from_str(&snapshot_json).expect("snapshot metadata");

        let list_ptr = libresync_backup_list(handle, adapter_id.as_ptr());
        let list_json = take_string(list_ptr).expect("list json");
        let snapshots: Vec<libresync::SnapshotMetadata> =
            serde_json::from_str(&list_json).expect("snapshot list");
        assert!(!snapshots.is_empty());

        let snapshot_id = CString::new(metadata.id.clone()).expect("snapshot id");
        let preview_ptr =
            libresync_backup_preview(handle, adapter_id.as_ptr(), snapshot_id.as_ptr());
        let preview_json = take_string(preview_ptr).expect("preview json");
        let preview_value: serde_json::Value =
            serde_json::from_str(&preview_json).expect("preview value");
        assert!(preview_value.is_object());

        assert!(libresync_backup_restore(
            handle,
            adapter_id.as_ptr(),
            snapshot_id.as_ptr()
        ));

        assert!(libresync_backup_prune(handle, adapter_id.as_ptr(), 1, 0));

        libresync_engine_free(handle);
    }

    #[test]
    fn ffi_error_paths_for_link_and_sync() {
        let (_tempdir, handle) = create_engine(None);

        let bad_addr = CString::new("nope").expect("bad addr");
        let result = libresync_engine_link(handle, bad_addr.as_ptr());
        assert!(result.is_null());
        assert!(last_error().is_some());

        let adapter_id = CString::new("missing").expect("adapter id");
        assert!(!libresync_engine_sync_now(
            handle,
            bad_addr.as_ptr(),
            adapter_id.as_ptr()
        ));
        assert!(last_error().is_some());

        let discover_ptr = libresync_engine_discover(handle, 1);
        if let Some(discover_json) = take_string(discover_ptr) {
            let discover_value: serde_json::Value =
                serde_json::from_str(&discover_json).expect("discover json parse");
            assert!(discover_value.is_array());
        } else {
            assert!(last_error().is_some());
        }

        libresync_engine_free(handle);
    }

    #[test]
    fn ffi_pairing_secret_is_optional_and_trimmed() {
        let (_tempdir, handle_ptr) = create_engine_with_secret(Some("shared-secret"));
        let handle = unsafe { &*handle_ptr };
        assert_eq!(
            handle.handler.pairing_secret.as_deref(),
            Some("shared-secret")
        );
        libresync_engine_free(handle_ptr);

        let (_tempdir, handle_ptr) = create_engine_with_secret(Some("   "));
        let handle = unsafe { &*handle_ptr };
        assert!(handle.handler.pairing_secret.is_none());
        libresync_engine_free(handle_ptr);
    }

    #[test]
    fn ffi_generates_keys_and_registers_sqlite_logical_adapter() {
        let app_key_ptr = libresync_generate_app_key();
        let app_key = take_string(app_key_ptr).expect("app key");
        let key_bytes = BASE64.decode(app_key.as_bytes()).expect("decode app key");
        assert_eq!(key_bytes.len(), 32);

        let device_id = CString::new("device-a").unwrap();
        let app_id = CString::new("app-a").unwrap();
        let user_id = CString::new("user-a").unwrap();
        let device_json_ptr =
            libresync_generate_device_keys(device_id.as_ptr(), app_id.as_ptr(), user_id.as_ptr());
        let device_json = take_string(device_json_ptr).expect("device keys");
        let keys: serde_json::Value = serde_json::from_str(&device_json).expect("keys");
        assert!(
            keys.get("device_cert_der")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .len()
                > 10
        );
        assert!(
            keys.get("device_key_der")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .len()
                > 10
        );
        assert!(
            keys.get("fingerprint")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .len()
                > 10
        );

        let (config_json, state_path, tempdir) = build_config(None, true, None);
        let handle = libresync_engine_create(config_json.as_ptr(), state_path.as_ptr());
        assert!(!handle.is_null(), "engine should be created");

        let sqlite_path = tempdir.path().join("records.sqlite");
        std::fs::write(&sqlite_path, []).expect("write sqlite");
        let mapping = serde_json::json!({
            "data_table": "notes",
            "id_column": "id",
            "schema": "schema",
            "entity": "Note",
            "fields": [
                { "column": "title", "field": "title" }
            ]
        });
        let adapter_id = CString::new("logical").unwrap();
        let namespace = CString::new("notes").unwrap();
        let sqlite_path_c = CString::new(sqlite_path.to_string_lossy().to_string()).unwrap();
        let mapping_c = CString::new(mapping.to_string()).unwrap();
        assert!(libresync_engine_register_sqlite_logical_adapter(
            handle,
            adapter_id.as_ptr(),
            namespace.as_ptr(),
            sqlite_path_c.as_ptr(),
            mapping_c.as_ptr(),
        ));

        libresync_engine_free(handle);
    }

    #[test]
    fn ffi_sqlite_logical_adapter_rejects_bad_mapping() {
        let (config_json, state_path, _tempdir) = build_config(None, true, None);
        let handle = libresync_engine_create(config_json.as_ptr(), state_path.as_ptr());
        assert!(!handle.is_null(), "engine should be created");

        let adapter_id = CString::new("logical").unwrap();
        let namespace = CString::new("notes").unwrap();
        let sqlite_path_c = CString::new("missing.sqlite").unwrap();
        let mapping_c = CString::new("not json").unwrap();
        assert!(!libresync_engine_register_sqlite_logical_adapter(
            handle,
            adapter_id.as_ptr(),
            namespace.as_ptr(),
            sqlite_path_c.as_ptr(),
            mapping_c.as_ptr(),
        ));
        assert!(last_error().is_some());

        libresync_engine_free(handle);
    }

    #[test]
    fn ffi_registers_sqlite_logical_adapter_with_mapping_array() {
        let (config_json, state_path, tempdir) = build_config(None, true, None);
        let handle = libresync_engine_create(config_json.as_ptr(), state_path.as_ptr());
        assert!(!handle.is_null(), "engine should be created");

        let sqlite_path = tempdir.path().join("records.sqlite");
        std::fs::write(&sqlite_path, []).expect("write sqlite");
        let mappings = serde_json::json!([
            {
                "data_table": "notes",
                "id_column": "id",
                "schema": "schema",
                "entity": "Note",
                "fields": [
                    { "column": "title", "field": "title" }
                ]
            },
            {
                "data_table": "tasks",
                "id_column": "id",
                "schema": "schema",
                "entity": "Task",
                "fields": [
                    { "column": "done", "field": "done", "encoding": "bool" }
                ]
            }
        ]);
        let adapter_id = CString::new("logical").unwrap();
        let namespace = CString::new("notes").unwrap();
        let sqlite_path_c = CString::new(sqlite_path.to_string_lossy().to_string()).unwrap();
        let mapping_c = CString::new(mappings.to_string()).unwrap();
        assert!(libresync_engine_register_sqlite_logical_adapter(
            handle,
            adapter_id.as_ptr(),
            namespace.as_ptr(),
            sqlite_path_c.as_ptr(),
            mapping_c.as_ptr(),
        ));

        libresync_engine_free(handle);
    }

    #[test]
    fn ffi_event_queue_reports_listener_and_async_sync() {
        let (_tempdir, handle) = create_engine(Some("127.0.0.1:0"));
        assert_eq!(libresync_abi_version(), 2);
        assert!(libresync_engine_event_next(handle).is_null());
        assert!(last_error().is_none());
        let fd = libresync_engine_event_fd(handle);
        assert!(fd >= 0 || !cfg!(unix));

        assert!(libresync_engine_start_listening(handle));
        let started = take_string(libresync_engine_event_next(handle)).expect("event");
        let value: serde_json::Value = serde_json::from_str(&started).expect("json");
        assert_eq!(value["type"], "listener_started");
        assert!(value["address"].as_str().unwrap_or("").starts_with("127.0.0.1:"));

        // Async sync against a closed port fails quickly and reports a ticket.
        let address = CString::new("127.0.0.1:1").expect("addr");
        let adapter_id = CString::new("missing").expect("adapter");
        let ticket = libresync_engine_sync_async(handle, address.as_ptr(), adapter_id.as_ptr());
        assert!(ticket > 0);
        let mut finished = None;
        for _ in 0..50 {
            let ptr = libresync_engine_event_wait(handle, 200);
            if let Some(json) = take_string(ptr) {
                let value: serde_json::Value = serde_json::from_str(&json).expect("json");
                if value["type"] == "task_finished" {
                    finished = Some(value);
                    break;
                }
            }
        }
        let finished = finished.expect("task_finished event");
        assert_eq!(finished["ticket"], ticket);
        assert_eq!(finished["result"]["ok"], false);
        assert!(!libresync_engine_cancel(handle, ticket));

        let bad = CString::new("nope").expect("addr");
        assert_eq!(
            libresync_engine_sync_async(handle, bad.as_ptr(), adapter_id.as_ptr()),
            0
        );
        assert!(last_error().is_some());

        assert!(libresync_engine_stop_listening(handle));
        libresync_engine_free(handle);
        assert_eq!(libresync_engine_event_fd(std::ptr::null_mut()), -1);
        assert!(libresync_engine_event_next(std::ptr::null_mut()).is_null());
        assert!(libresync_engine_event_wait(std::ptr::null_mut(), 1).is_null());
        assert_eq!(libresync_engine_sync_async(std::ptr::null_mut(), bad.as_ptr(), adapter_id.as_ptr()), 0);
        assert!(!libresync_engine_cancel(std::ptr::null_mut(), 1));
    }

    #[test]
    fn ffi_event_json_covers_every_variant() {
        let device = DeviceInfo {
            identity: Identity::new("d", "a", "u"),
            address: Some("127.0.0.1:5".parse().expect("addr")),
            last_seen: None,
            linked: true,
            fingerprint: Some("ff".to_string()),
        };
        let request = libresync::LinkingRequest {
            device: device.clone(),
            code: Some("1234".to_string()),
        };
        let events = vec![
            Event::LinkingRequested { request: request.clone() },
            Event::LinkingDecisionRequired { request },
            Event::SyncStarted { device: device.clone(), adapter_id: "x".to_string() },
            Event::SyncFinished {
                device: device.clone(),
                adapter_id: "x".to_string(),
                result: SyncResult::Failed("boom".to_string()),
            },
            Event::DeviceSeen { device: device.clone() },
            Event::DeviceOffline { device: device.clone() },
            Event::Error { message: "m".to_string() },
            Event::FingerprintChanged { device: device.clone(), fingerprint: "ff".to_string() },
            Event::InboundSync { device: device.clone(), applied: 3 },
            Event::SyncStats {
                device: device.clone(),
                adapter_id: "x".to_string(),
                stats: libresync::SyncStats::default(),
            },
            Event::LinkFinished {
                address: "127.0.0.1:5".parse().expect("addr"),
                device: Some(device.clone()),
                error: None,
            },
            Event::DiscoveryFinished { devices: vec![device] },
            Event::ListenerStarted { address: "127.0.0.1:5".parse().expect("addr") },
            Event::ListenerStopped,
            Event::TaskFinished { ticket: 9, result: SyncResult::Success },
        ];
        let mut types = std::collections::HashSet::new();
        for event in &events {
            let json = event_to_json(event);
            let kind = json["type"].as_str().expect("type").to_string();
            assert_ne!(kind, "unknown");
            types.insert(kind);
        }
        assert_eq!(types.len(), events.len());
    }
}
