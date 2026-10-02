//! Versioned managed boundary. Managed handles are registry IDs, never pointers.
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use libresync::{DeviceMetadata, Error, KeyStore, Result, Session, SessionConfig};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    ffi::{c_void, CStr, CString},
    os::raw::c_char,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Duration,
};
const VERSION: u32 = 1;
const MAX_JSON: usize = 128 * 1024 * 1024;
struct Handle {
    _keys: Arc<dyn KeyStore>,
    session: Session,
    events: Mutex<flume::Receiver<libresync::SessionEvent>>,
    subscriptions: Mutex<HashMap<u64, flume::Receiver<libresync::SessionEvent>>>,
}
static HANDLES: OnceLock<Mutex<HashMap<u64, Arc<Handle>>>> = OnceLock::new();
static NEXT: AtomicU64 = AtomicU64::new(1);
fn handles() -> &'static Mutex<HashMap<u64, Arc<Handle>>> {
    HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}
fn error(code: &str, message: impl ToString) -> Value {
    json!({"abi":VERSION,"ok":false,"error":{"code":code,"message":message.to_string()}})
}
fn ok(value: impl Serialize) -> Value {
    json!({"abi":VERSION,"ok":true,"value":value})
}
fn convert(e: Error) -> Value {
    let code = match &e {
        Error::Managed { code, .. } => format!("{code:?}"),
        Error::Cancelled => "Cancelled".into(),
        _ => "OperationFailed".into(),
    };
    error(&code, e)
}
fn invalid(message: &str) -> Error {
    Error::Managed {
        code: libresync::SessionErrorCode::InvalidConfiguration,
        message: message.into(),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    policy: String,
    #[serde(default)]
    manifest: Option<libresync::AppManifest>,
    state_dir: String,
    display_name: String,
    device_kind: String,
    #[serde(default = "role")]
    role: String,
    #[serde(default)]
    listen: Option<String>,
    #[serde(default)]
    advertise: bool,
    #[serde(default = "io_timeout")]
    authenticated_io_timeout_ms: u64,
}
fn role() -> String {
    "device".into()
}
fn io_timeout() -> u64 {
    60_000
}
pub(super) fn open(input: Value, keys: Arc<dyn KeyStore>) -> Value {
    let config: Config = match serde_json::from_value(input) {
        Ok(v) => v,
        Err(e) => return error("InvalidConfiguration", e),
    };
    let support = match config.policy.as_str() {
        "notes" => libresync::companion::default_catalog()
            .into_iter()
            .find(|a| a.manifest == libresync::companion::notes_manifest()),
        "records" => config
            .manifest
            .clone()
            .filter(|m| {
                m.validate().is_ok()
                    && m.adapters
                        .iter()
                        .all(|a| a.schema == "logical-records-v1" && a.transactional)
            })
            .map(|manifest| libresync::companion::AppSupport {
                adapters: manifest
                    .adapters
                    .iter()
                    .map(|d| {
                        Arc::new(libresync::RecordsAdapter::new(d.clone()))
                            as Arc<dyn libresync::ManagedAdapter>
                    })
                    .collect(),
                manifest,
            }),
        "momentum" => libresync::companion::default_catalog()
            .into_iter()
            .find(|a| a.manifest == libresync::companion::momentum_manifest()),
        _ => None,
    };
    let Some(support) = support else {
        return error(
            "InvalidAdapter",
            "Unsupported policy; register a reviewed Rust ManagedAdapter or explicitly select records/notes/momentum",
        );
    };
    if config
        .manifest
        .as_ref()
        .is_some_and(|m| m != &support.manifest)
    {
        return error(
            "InvalidAdapter",
            "Manifest differs from the selected verified policy",
        );
    }
    let result = (|| -> Result<Session> {
        let mut c = SessionConfig::new(
            config.state_dir,
            DeviceMetadata {
                display_name: config.display_name,
                device_kind: config.device_kind,
                role: config.role,
                manifest: support.manifest,
            },
        );
        c.advertise = config.advertise;
        c.authenticated_io_timeout = Duration::from_millis(config.authenticated_io_timeout_ms);
        if let Some(addr) = config.listen {
            c.listen = addr
                .parse()
                .map_err(|_| invalid("invalid listen address"))?;
        }
        for adapter in support.adapters {
            c = c.with_adapter(adapter)?;
        }
        Session::open(c, keys.clone())
    })();
    match result {
        Err(e) => convert(e),
        Ok(session) => {
            let events = match session.subscribe() {
                Ok(v) => v,
                Err(e) => return convert(e),
            };
            let id = match NEXT
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            {
                Ok(id) => id,
                Err(_) => return error("Busy", "managed handle space exhausted"),
            };
            if id == 0 || id == u64::MAX {
                return error("Busy", "managed handle space exhausted");
            }
            match handles().lock() {
                Ok(mut map) => {
                    map.insert(
                        id,
                        Arc::new(Handle {
                            _keys: keys,
                            session,
                            events: Mutex::new(events),
                            subscriptions: Mutex::new(HashMap::new()),
                        }),
                    );
                    ok(id)
                }
                Err(_) => error("Busy", "handle registry unavailable"),
            }
        }
    }
}
pub(super) fn close(id: u64) -> Value {
    let handle = match handles().lock() {
        Ok(mut m) => m.remove(&id),
        Err(_) => return error("Busy", "handle registry unavailable"),
    };
    if let Some(h) = handle {
        match h.session.shutdown() {
            Ok(()) => ok(()),
            Err(e) => convert(e),
        }
    } else {
        ok(())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InboxAcknowledgement {
    revision: u64,
    receipts: std::collections::BTreeMap<String, libresync::ManagedReceipt>,
    // Accepted for pre-release C callers, but native SDKs send no record body.
    #[serde(default, rename = "records")]
    _records: Option<serde::de::IgnoredAny>,
}
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    PlatformAdvertisement,
    IngestDiscovery {
        peer: libresync::DiscoveredPeer,
    },
    WithdrawDiscovery {
        device_id: String,
    },
    Cancel,
    Start,
    Shutdown,
    Pause,
    Resume,
    Wake,
    Snapshot,
    Discovery,
    PendingPairings,
    RecoverPairing,
    ClosePairing,
    Inbox,
    Set {
        adapter: String,
        id: String,
        value: String,
    },
    Delete {
        adapter: String,
        id: String,
    },
    Get {
        adapter: String,
        id: String,
    },
    Records {
        adapter: String,
    },
    Import {
        records: Vec<libresync::ManagedRecord>,
    },
    AcknowledgeInbox {
        inbox: InboxAcknowledgement,
    },
    Invitation {
        ttl_ms: u64,
        #[serde(default)]
        code: bool,
    },
    Connect {
        invitation: String,
    },
    ConnectCode {
        peer: libresync::DiscoveredPeer,
        code: String,
    },
    Repair {
        peer: String,
        invitation: String,
    },
    RemovePeer {
        peer: String,
    },
    PausePeer {
        peer: String,
    },
    ResumePeer {
        peer: String,
    },
    BootstrapPreview {
        peer: String,
    },
    ResolveBootstrap {
        preview: libresync::BootstrapPreview,
        decision: libresync::BootstrapDecision,
    },
    AcknowledgeApplied {
        peer: String,
        receipt: libresync::ManagedReceipt,
    },
    StoredReceipt {
        peer: String,
    },
    Recovery,
    RecoverySizes,
    PruneRecovery {
        retain: usize,
    },
    Inspections,
    Evidence {
        platform: String,
        permission: String,
        state: libresync::PermissionState,
    },
    Subscribe,
    Unsubscribe {
        subscription: u64,
    },
    Events {
        timeout_ms: u64,
        #[serde(default)]
        subscription: Option<u64>,
    },
}
fn invitation(encoded: &str) -> Result<libresync::SessionInvitation> {
    // Version hint before strict decoding gives actionable old-version guidance.
    if encoded.len() > 16 * 1024 {
        return Err(invalid("invitation too large"));
    }
    let hint: Value = serde_json::from_str(encoded)?;
    if hint["invitation"]["version"].as_u64() != Some(libresync::PAIRING_VERSION as u64) {
        return Err(Error::Managed {
            code: libresync::SessionErrorCode::IncompatibleSchema,
            message: "Upgrade both devices: unsupported pairing invitation version".into(),
        });
    }
    libresync::SessionInvitation::decode(encoded)
}
pub(super) fn dispatch(id: u64, input: Value) -> Value {
    let handle = match handles().lock() {
        Ok(m) => match m.get(&id) {
            Some(h) => h.clone(),
            None => return error("Closed", "Session closed"),
        },
        Err(_) => return error("Busy", "handle registry unavailable"),
    };
    let command: Command = match serde_json::from_value(input) {
        Ok(v) => v,
        Err(e) => return error("InvalidOperation", e),
    };
    let s = &handle.session;
    let result = (|| -> Result<Value> {
        Ok(match command {
            Command::PlatformAdvertisement => json!(s.platform_advertisement()?),
            Command::IngestDiscovery { peer } => {
                s.ingest_platform_discovery(peer)?;
                Value::Null
            }
            Command::WithdrawDiscovery { device_id } => {
                s.withdraw_platform_discovery(&device_id)?;
                Value::Null
            }
            Command::Cancel => {
                s.cancel_operations()?;
                Value::Null
            }
            Command::Start => json!(s.start()?),
            Command::Shutdown => {
                s.shutdown()?;
                Value::Null
            }
            Command::Pause => {
                s.pause()?;
                Value::Null
            }
            Command::Resume => json!(s.resume()?),
            Command::Wake => {
                s.wake()?;
                Value::Null
            }
            Command::Snapshot => json!(s.snapshot()?),
            Command::Discovery => json!(s.discovered_peers()?),
            Command::PendingPairings => json!(s.pending_pairings()?),
            Command::RecoverPairing => {
                s.recover_pairing()?;
                Value::Null
            }
            Command::ClosePairing => {
                s.close_pairing()?;
                Value::Null
            }
            Command::Set { adapter, id, value } => {
                let data = BASE64.decode(value).map_err(|_| Error::Managed {
                    code: libresync::SessionErrorCode::InvalidRecord,
                    message: "Invalid base64 record".into(),
                })?;
                s.set(&adapter, &id, data)?;
                Value::Null
            }
            Command::Delete { adapter, id } => {
                s.delete(&adapter, &id)?;
                Value::Null
            }
            Command::Get { adapter, id } => json!(s.get(&adapter, &id)?.map(|v| BASE64.encode(v))),
            Command::Records { adapter } => json!(s.records(&adapter)?),
            Command::Import { records } => {
                s.import_records(&records)?;
                Value::Null
            }
            Command::Inbox => json!(s.application_inbox()?),
            Command::AcknowledgeInbox { inbox } => {
                s.acknowledge_inbox(&libresync::ApplicationInbox {
                    revision: inbox.revision,
                    receipts: inbox.receipts,
                    records: Vec::new(),
                })?;
                Value::Null
            }
            Command::Invitation { ttl_ms, code } => {
                let ttl = Duration::from_millis(ttl_ms);
                let i = if code {
                    s.create_code_invitation(ttl)?
                } else {
                    s.create_invitation(ttl)?
                };
                json!(i)
            }
            Command::Connect { invitation: i } => json!(s.connect(&invitation(&i)?)?),
            Command::ConnectCode { peer, code } => json!(s.connect_code(&peer, &code)?),
            Command::Repair {
                peer,
                invitation: i,
            } => json!(s.repair_peer(&peer, &invitation(&i)?)?),
            Command::RemovePeer { peer } => {
                s.remove_peer(&peer)?;
                Value::Null
            }
            Command::PausePeer { peer } => {
                s.pause_peer(&peer)?;
                Value::Null
            }
            Command::ResumePeer { peer } => {
                s.resume_peer(&peer)?;
                Value::Null
            }
            Command::BootstrapPreview { peer } => json!(s.bootstrap_preview(&peer)?),
            Command::ResolveBootstrap { preview, decision } => {
                s.resolve_bootstrap(&preview, decision)?;
                Value::Null
            }
            Command::AcknowledgeApplied { peer, receipt } => {
                s.acknowledge_applied(&peer, &receipt)?;
                Value::Null
            }
            Command::StoredReceipt { peer } => json!(s.stored_inbound_receipt(&peer)?),
            Command::Recovery => json!(s.recovery_snapshots()?),
            Command::RecoverySizes => json!(s.recovery_snapshot_sizes()?),
            Command::PruneRecovery { retain } => {
                s.prune_recovery_snapshots(retain)?;
                Value::Null
            }
            Command::Inspections => json!(s.adapter_inspections()?),
            Command::Evidence {
                platform,
                permission,
                state,
            } => {
                s.report_platform_evidence(platform, permission, state);
                Value::Null
            }
            Command::Subscribe => {
                let mut subscriptions = handle
                    .subscriptions
                    .lock()
                    .map_err(|_| invalid("subscriptions unavailable"))?;
                if subscriptions.len() >= 64 {
                    return Err(Error::Managed {
                        code: libresync::SessionErrorCode::Busy,
                        message: "subscription capacity reached".into(),
                    });
                }
                let id = NEXT
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                    .map_err(|_| invalid("subscription ID exhausted"))?;
                subscriptions.insert(id, s.subscribe()?);
                json!(id)
            }
            Command::Unsubscribe { subscription } => {
                handle
                    .subscriptions
                    .lock()
                    .map_err(|_| invalid("subscriptions unavailable"))?
                    .remove(&subscription);
                Value::Null
            }
            Command::Events {
                timeout_ms,
                subscription,
            } => {
                if timeout_ms > 1000 {
                    return Err(invalid("event poll maximum 1000 ms"));
                }
                let receiver = if let Some(id) = subscription {
                    handle
                        .subscriptions
                        .lock()
                        .map_err(|_| invalid("subscriptions unavailable"))?
                        .get(&id)
                        .cloned()
                        .ok_or_else(|| invalid("subscription closed"))?
                } else {
                    handle
                        .events
                        .lock()
                        .map_err(|_| invalid("event queue unavailable"))?
                        .clone()
                };
                let mut events = Vec::new();
                if let Ok(e) = receiver.recv_timeout(Duration::from_millis(timeout_ms)) {
                    events.push(e);
                }
                events.extend(receiver.try_iter().take(255));
                json!(events)
            }
        })
    })();
    match result {
        Ok(v) => ok(v),
        Err(e) => convert(e),
    }
}
/// A callback returns 0 success, 1 absent (get only), -1 unavailable/error.
/// Returned output is callback-owned until release_buffer. Context lives until
/// release_context after close AND all in-flight calls. Callbacks may run on any
/// native worker thread; serialize secure storage internally, never reenter.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct KeyCallbacks {
    pub context: *mut c_void,
    pub call: Option<
        unsafe extern "C" fn(
            *mut c_void,
            u32,
            *const c_char,
            *const u8,
            usize,
            *mut *const u8,
            *mut usize,
        ) -> i32,
    >,
    pub release_buffer: Option<unsafe extern "C" fn(*mut c_void, *const u8, usize)>,
    pub release_context: Option<unsafe extern "C" fn(*mut c_void)>,
}
struct CallbackKeys(KeyCallbacks);
// SAFETY: the ABI requires thread-safe callbacks and transferable context.
unsafe impl Send for CallbackKeys {}
unsafe impl Sync for CallbackKeys {}
impl Drop for CallbackKeys {
    fn drop(&mut self) {
        if let Some(release) = self.0.release_context {
            unsafe { release(self.0.context) }
        }
    }
}
impl CallbackKeys {
    fn invoke(&self, operation: u32, name: &str, value: &[u8]) -> Result<Option<Vec<u8>>> {
        let name = CString::new(name).map_err(|_| invalid("invalid key item"))?;
        let mut output = std::ptr::null();
        let mut length = 0;
        let call = self
            .0
            .call
            .ok_or_else(|| invalid("secure storage callback missing"))?;
        let status = unsafe {
            call(
                self.0.context,
                operation,
                name.as_ptr(),
                value.as_ptr(),
                value.len(),
                &mut output,
                &mut length,
            )
        };
        let result = if status == 1 && operation == 0 {
            Ok(None)
        } else if status != 0 {
            Err(Error::Managed {
                code: libresync::SessionErrorCode::StorageUnavailable,
                message: "Secure storage unavailable; identity was not regenerated".into(),
            })
        } else if operation != 0 {
            Ok(None)
        } else if output.is_null() || length > 1024 * 1024 {
            Err(invalid("invalid secure storage callback buffer"))
        } else {
            Ok(Some(
                unsafe { std::slice::from_raw_parts(output, length) }.to_vec(),
            ))
        };
        if !output.is_null() {
            if let Some(release) = self.0.release_buffer {
                unsafe { release(self.0.context, output, length) }
            }
        }
        result
    }
}
impl KeyStore for CallbackKeys {
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>> {
        self.invoke(0, name, &[])
    }
    fn set(&self, name: &str, value: &[u8]) -> Result<()> {
        self.invoke(1, name, value).map(|_| ())
    }
    fn delete(&self, name: &str) -> Result<()> {
        self.invoke(2, name, &[]).map(|_| ())
    }
}
fn string(value: Value) -> *mut c_char {
    match CString::new(value.to_string()) {
        Ok(v) => v.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}
unsafe fn input(ptr: *const c_char) -> std::result::Result<Value, Value> {
    if ptr.is_null() {
        return Err(error("InvalidOperation", "JSON pointer is null"));
    }
    let bytes = unsafe { CStr::from_ptr(ptr) }.to_bytes();
    if bytes.len() > MAX_JSON {
        return Err(error("InvalidRecord", "Serialized request exceeds 128 MiB"));
    }
    serde_json::from_slice(bytes).map_err(|e| error("InvalidOperation", e))
}
#[no_mangle]
pub extern "C" fn libresync_managed_abi_version() -> u32 {
    VERSION
}
/// Returns owned JSON envelope. Free with libresync_string_free.
/// # Safety
/// JSON must be NUL terminated, callbacks valid/thread-safe for the handle lifetime.
#[no_mangle]
pub unsafe extern "C" fn libresync_session_open(
    json: *const c_char,
    callbacks: KeyCallbacks,
) -> *mut c_char {
    let keys = Arc::new(CallbackKeys(callbacks));
    if callbacks.call.is_none() || callbacks.release_buffer.is_none() {
        return string(error(
            "StorageUnavailable",
            "Secure storage requires call and release_buffer callbacks",
        ));
    }
    string(match unsafe { input(json) } {
        Ok(v) => open(v, keys),
        Err(e) => e,
    })
}
/// # Safety
/// JSON must be a readable NUL-terminated string for this call only.
#[no_mangle]
pub unsafe extern "C" fn libresync_session_call(id: u64, json: *const c_char) -> *mut c_char {
    string(match unsafe { input(json) } {
        Ok(v) => dispatch(id, v),
        Err(e) => e,
    })
}
#[no_mangle]
pub extern "C" fn libresync_session_close(id: u64) -> *mut c_char {
    string(close(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    struct CallbackContext {
        values: Mutex<HashMap<String, Vec<u8>>>,
        drops: Arc<std::sync::atomic::AtomicUsize>,
        releases: Arc<std::sync::atomic::AtomicUsize>,
    }
    unsafe extern "C" fn test_key_call(
        ctx: *mut c_void,
        op: u32,
        name: *const c_char,
        value: *const u8,
        len: usize,
        out: *mut *const u8,
        outlen: *mut usize,
    ) -> i32 {
        let context = unsafe { &*(ctx as *const CallbackContext) };
        let name = unsafe { CStr::from_ptr(name) }
            .to_string_lossy()
            .into_owned();
        let mut values = context.values.lock().unwrap();
        match op {
            0 => {
                let Some(bytes) = values.get(&name) else {
                    return 1;
                };
                let bytes = bytes.clone().into_boxed_slice();
                unsafe {
                    *outlen = bytes.len();
                    *out = Box::into_raw(bytes) as *const u8
                };
                0
            }
            1 => {
                values.insert(
                    name,
                    unsafe { std::slice::from_raw_parts(value, len) }.to_vec(),
                );
                0
            }
            2 => {
                values.remove(&name);
                0
            }
            _ => -1,
        }
    }
    unsafe extern "C" fn test_release_buffer(ctx: *mut c_void, bytes: *const u8, len: usize) {
        unsafe {
            drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                bytes as *mut u8,
                len,
            )))
        };
        unsafe { &*(ctx as *const CallbackContext) }
            .releases
            .fetch_add(1, Ordering::SeqCst);
    }
    unsafe extern "C" fn test_release_context(ctx: *mut c_void) {
        let context = unsafe { Box::from_raw(ctx as *mut CallbackContext) };
        context.drops.fetch_add(1, Ordering::SeqCst);
    }
    #[test]
    fn close_does_not_wait_for_a_subscription_consumer() {
        let dir = tempfile::tempdir().unwrap();
        let opened = open(
            json!({"policy":"notes","state_dir":dir.path(),"display_name":"Poll close","device_kind":"desktop","advertise":false}),
            Arc::new(libresync::MemoryKeyStore::new()),
        );
        let id = opened["value"].as_u64().unwrap();
        let sub = dispatch(id, json!({"op":"subscribe"}))["value"]
            .as_u64()
            .unwrap();
        let poll = std::thread::spawn(move || {
            dispatch(
                id,
                json!({"op":"events","subscription":sub,"timeout_ms":1000}),
            )
        });
        std::thread::sleep(Duration::from_millis(50));
        let start = std::time::Instant::now();
        assert_eq!(close(id)["ok"], true);
        assert!(start.elapsed() < Duration::from_millis(500));
        let _ = poll.join().unwrap();
        assert!(start.elapsed() < Duration::from_millis(1100));
        assert_eq!(
            dispatch(id, json!({"op":"subscribe"}))["error"]["code"],
            "Closed"
        );
    }
    #[test]
    fn exported_failed_open_releases_callback_context_exactly_once() {
        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let context = Box::into_raw(Box::new(CallbackContext {
            values: Mutex::new(HashMap::new()),
            drops: drops.clone(),
            releases: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }));
        let config = CString::new("{}").unwrap();
        let response = unsafe {
            libresync_session_open(
                config.as_ptr(),
                KeyCallbacks {
                    context: context.cast(),
                    call: Some(test_key_call),
                    release_buffer: Some(test_release_buffer),
                    release_context: Some(test_release_context),
                },
            )
        };
        let failed: Value =
            serde_json::from_str(unsafe { CStr::from_ptr(response) }.to_str().unwrap()).unwrap();
        super::super::libresync_string_free(response);
        assert_eq!(failed["ok"], false);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn callback_buffers_and_context_remain_owned_until_last_inflight_handle() {
        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let releases = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let context = Box::into_raw(Box::new(CallbackContext {
            values: Mutex::new(HashMap::new()),
            drops: drops.clone(),
            releases: releases.clone(),
        }));
        let keys = std::mem::ManuallyDrop::new(CallbackKeys(KeyCallbacks {
            context: context.cast(),
            call: Some(test_key_call),
            release_buffer: Some(test_release_buffer),
            release_context: Some(test_release_context),
        }));
        keys.set("buffer", b"value").unwrap();
        assert_eq!(keys.get("buffer").unwrap(), Some(b"value".to_vec()));
        assert_eq!(releases.load(Ordering::SeqCst), 1);
        let dir = tempfile::tempdir().unwrap();
        let config=CString::new(json!({"policy":"notes","state_dir":dir.path(),"display_name":"Callbacks","device_kind":"desktop","advertise":false}).to_string()).unwrap();
        let response = unsafe { libresync_session_open(config.as_ptr(), keys.0) };
        let opened: Value =
            serde_json::from_str(unsafe { CStr::from_ptr(response) }.to_str().unwrap()).unwrap();
        super::super::libresync_string_free(response);
        let id = opened["value"].as_u64().unwrap();

        let inflight = handles().lock().unwrap().get(&id).unwrap().clone();
        let response = libresync_session_close(id);
        let closed: Value =
            serde_json::from_str(unsafe { CStr::from_ptr(response) }.to_str().unwrap()).unwrap();
        super::super::libresync_string_free(response);
        assert_eq!(closed["ok"], true);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert_eq!(
            dispatch(id, json!({"op":"snapshot"}))["error"]["code"],
            "Closed"
        );
        drop(inflight);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn explicit_records_policy_accepts_custom_app_only_with_reviewed_schema() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = json!({"policy":"records","manifest":{"app_id":"com.example.Journal","display_name":"Journal","schema_version":1,"adapters":[{"id":"entries","namespace":"com.example.Journal","schema":"logical-records-v1","transactional":true}]},"state_dir":dir.path(),"display_name":"Laptop","device_kind":"desktop"});
        let r = open(
            c.clone(),
            std::sync::Arc::new(libresync::MemoryKeyStore::new()),
        );
        assert_eq!(r["ok"], true);
        close(r["value"].as_u64().unwrap());
        c["manifest"]["adapters"][0]["schema"] = json!("custom-schema");
        assert_eq!(
            open(c, std::sync::Arc::new(libresync::MemoryKeyStore::new()))["error"]["code"],
            "InvalidAdapter"
        );
    }
    #[test]
    fn platform_advertisement_uses_real_port_and_only_public_pairing_hints() {
        let dir = tempfile::tempdir().unwrap();
        let r = open(
            json!({"policy":"notes","state_dir":dir.path(),"display_name":"Friendly laptop","device_kind":"desktop","advertise":false}),
            std::sync::Arc::new(libresync::MemoryKeyStore::new()),
        );
        let id = r["value"].as_u64().unwrap();
        let address = dispatch(id, json!({"op":"start"}))["value"]
            .as_str()
            .unwrap()
            .to_string();
        let ad = dispatch(id, json!({"op":"platform_advertisement"}));
        assert_eq!(
            ad["value"]["port"].as_u64().unwrap(),
            address.parse::<std::net::SocketAddr>().unwrap().port() as u64
        );
        assert_eq!(ad["value"]["txt"]["name"], "Friendly laptop");
        dispatch(id, json!({"op":"invitation","ttl_ms":30000,"code":true}));
        let ad = dispatch(id, json!({"op":"platform_advertisement"}));
        assert_eq!(ad["value"]["txt"]["pair_version"], "2");
        assert!(ad["value"]["txt"].get("secret").is_none());
        dispatch(id, json!({"op":"close_pairing"}));
        assert!(
            dispatch(id, json!({"op":"platform_advertisement"}))["value"]["txt"]
                .get("pair_id")
                .is_none()
        );
        close(id);
    }
    #[test]
    fn untrusted_platform_hints_do_not_rewrite_durable_journal() {
        let dir = tempfile::tempdir().unwrap();
        let r = open(
            json!({"policy":"notes","state_dir":dir.path(),"display_name":"A","device_kind":"desktop","advertise":false}),
            std::sync::Arc::new(libresync::MemoryKeyStore::new()),
        );
        let id = r["value"].as_u64().unwrap();
        dispatch(id, json!({"op":"start"}));
        let before = std::fs::read(dir.path().join("session.enc")).unwrap();
        let peer = json!({"identity":{"device_id":"unknown","app_id":"io.libresync.Notes","user_id":"managed"},"addresses":["127.0.0.1:9"],"advertisement":{"display_name":"Untrusted","device_kind":"desktop","role":"device","app_display_name":"Notes","schema_version":1,"contract_digest":"a".repeat(64)},"invitation":null});
        assert_eq!(
            dispatch(id, json!({"op":"ingest_discovery","peer":peer}))["ok"],
            true
        );
        assert_eq!(
            std::fs::read(dir.path().join("session.enc")).unwrap(),
            before
        );
        close(id);
    }
    #[test]
    fn independent_observers_receive_changes_and_one_unsubscribe_preserves_other() {
        let dir = tempfile::tempdir().unwrap();
        let r = open(
            json!({"policy":"notes","state_dir":dir.path(),"display_name":"A","device_kind":"desktop","advertise":false}),
            std::sync::Arc::new(libresync::MemoryKeyStore::new()),
        );
        let id = r["value"].as_u64().unwrap();
        let a = dispatch(id, json!({"op":"subscribe"}));
        assert_eq!(a["ok"], true);
        let a = a["value"].as_u64().unwrap();
        let b = dispatch(id, json!({"op":"subscribe"}))["value"]
            .as_u64()
            .unwrap();
        dispatch(
            id,
            json!({"op":"set","adapter":"records","id":"one","value":"aGk="}),
        );
        for sub in [a, b] {
            assert!(
                dispatch(id, json!({"op":"events","timeout_ms":0,"subscription":sub}))["value"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("Changed"))
            );
        }
        dispatch(id, json!({"op":"unsubscribe","subscription":a}));
        dispatch(
            id,
            json!({"op":"set","adapter":"records","id":"two","value":"aGk="}),
        );
        assert!(
            dispatch(id, json!({"op":"events","timeout_ms":0,"subscription":b}))["value"]
                .as_array()
                .unwrap()
                .contains(&json!("Changed"))
        );
        close(id);
    }
    #[test]
    fn managed_unknown_and_closed_handles_are_typed_and_close_is_idempotent() {
        let r = dispatch(123456, json!({"op":"snapshot"}));
        assert_eq!(r["error"]["code"], "Closed");
        assert_eq!(close(123456)["ok"], true);
    }
    #[test]
    fn managed_refuses_unknown_policy_instead_of_generic_record_merge() {
        let r = open(
            json!({"policy":"custom-unreviewed","state_dir":"unused","display_name":"Test","device_kind":"desktop"}),
            std::sync::Arc::new(libresync::MemoryKeyStore::new()),
        );
        assert_eq!(r["error"]["code"], "InvalidAdapter");
    }
    #[test]
    fn managed_roundtrips_durable_records_and_closes_concurrently() {
        let dir = tempfile::tempdir().unwrap();
        let r = open(
            json!({"policy":"notes","state_dir":dir.path(),"display_name":"Test","device_kind":"desktop","advertise":false}),
            std::sync::Arc::new(libresync::MemoryKeyStore::new()),
        );
        let id = r["value"].as_u64().expect("opened session");
        assert_eq!(
            dispatch(
                id,
                json!({"op":"set","adapter":"records","id":"one","value":"aGk="})
            )["ok"],
            true
        );
        assert_eq!(
            dispatch(id, json!({"op":"records","adapter":"records"}))["value"][0]["value"],
            "aGk="
        );
        let worker = std::thread::spawn(move || dispatch(id, json!({"op":"snapshot"})));
        assert_eq!(close(id)["ok"], true);
        let result = worker.join().unwrap();
        assert!(result["ok"] == true || result["error"]["code"] == "Closed");
        assert_eq!(
            dispatch(id, json!({"op":"snapshot"}))["error"]["code"],
            "Closed"
        );
        assert_eq!(close(id)["ok"], true);
    }
    #[test]
    fn managed_storage_unavailable_never_falls_back_or_generates_identity() {
        struct Unavailable;
        impl libresync::KeyStore for Unavailable {
            fn get(&self, _: &str) -> libresync::Result<Option<Vec<u8>>> {
                Err(libresync::Error::Protocol("secure store locked".into()))
            }
            fn set(&self, _: &str, _: &[u8]) -> libresync::Result<()> {
                panic!("must not regenerate")
            }
            fn delete(&self, _: &str) -> libresync::Result<()> {
                panic!("must not delete")
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let r = open(
            json!({"policy":"notes","state_dir":dir.path(),"display_name":"Test","device_kind":"desktop"}),
            std::sync::Arc::new(Unavailable),
        );
        assert_eq!(r["ok"], false);
        assert!(!dir.path().join("identity.json").exists());
    }
}
