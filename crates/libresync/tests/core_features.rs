use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use libresync::{
    AppKey, DataAdapter, DeviceHandler, DeviceKeys, Engine, EngineConfig, Event, Identity,
    JsonFileAdapter, State, SyncListener, SyncResult, WatchedFileAdapter,
};

struct AllowAllHandler {
    app_id: String,
    keys: DeviceKeys,
    app_key: AppKey,
}

impl DeviceHandler for AllowAllHandler {
    fn app_id(&self) -> &str {
        &self.app_id
    }

    fn is_paired(&self, _identity: &Identity) -> bool {
        true
    }

    fn approve_pair(&self, _identity: &Identity) -> libresync::Result<bool> {
        Ok(true)
    }

    fn device_keys(&self) -> libresync::Result<DeviceKeys> {
        Ok(self.keys.clone())
    }

    fn app_key(&self) -> libresync::Result<AppKey> {
        Ok(self.app_key.clone())
    }
}

#[test]
fn engine_sync_emits_events() {
    let temp = tempfile::tempdir().expect("tempdir");
    let data_path = temp.path().join("data.json");
    std::fs::write(&data_path, b"{\"local\":true}").expect("write");

    let remote_identity = Identity::new("remote", "com.example.app", "remote-user");
    let remote_keys = DeviceKeys::generate(&remote_identity).expect("remote keys");
    let remote_app_key = AppKey::generate().expect("app key");
    let remote_state = Arc::new(Mutex::new(State::new(remote_identity.device_id.clone())));
    {
        let mut state = remote_state.lock().expect("state lock");
        state.set("file", b"{\"remote\":true}".to_vec());
    }
    let remote_handler = Arc::new(AllowAllHandler {
        app_id: remote_identity.app_id.clone(),
        keys: remote_keys.clone(),
        app_key: remote_app_key.clone(),
    });
    let listener = SyncListener::start(
        "127.0.0.1:0".parse::<SocketAddr>().expect("addr"),
        remote_identity.clone(),
        Arc::clone(&remote_state),
        remote_handler,
    )
    .expect("listener");
    let addr = listener.addr();

    let local_identity = Identity::new("local", "com.example.app", "local-user");
    let local_keys = DeviceKeys::generate(&local_identity).expect("local keys");
    let local_handler = Arc::new(AllowAllHandler {
        app_id: local_identity.app_id.clone(),
        keys: local_keys,
        app_key: remote_app_key,
    });
    let state = State::new(local_identity.device_id.clone());
    let mut engine = Engine::new(EngineConfig::new(local_identity), state, local_handler);
    engine
        .register_adapter(Arc::new(JsonFileAdapter::new("file", &data_path)))
        .expect("register adapter");
    let stream = engine.attach_event_channel();

    let device = engine.sync_now(addr, "file").expect("sync");
    assert_eq!(device.identity.device_id, "remote");
    assert!(device.fingerprint.is_some());

    let first = stream.recv().expect("event");
    match first {
        Event::SyncStarted { device, adapter_id } => {
            assert_eq!(adapter_id, "file");
            assert_eq!(device.identity.device_id, "remote");
        }
        _ => panic!("expected sync started event"),
    }

    let second = stream.recv().expect("event");
    match second {
        Event::SyncFinished {
            device,
            adapter_id,
            result,
        } => {
            assert_eq!(adapter_id, "file");
            assert_eq!(device.identity.device_id, "remote");
            assert!(matches!(result, SyncResult::Success));
        }
        _ => panic!("expected sync finished event"),
    }

    listener.shutdown().expect("shutdown");
}

#[test]
fn watched_file_adapter_creates_default_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let data_path = temp.path().join("data.json");
    let adapter = WatchedFileAdapter::new_json("file", &data_path).expect("adapter");
    let mut state = State::new("device");
    adapter.load_into_state(&mut state).expect("load");
    assert_eq!(state.get("file"), Some(b"{}".as_slice()));
    assert!(data_path.exists());
}
