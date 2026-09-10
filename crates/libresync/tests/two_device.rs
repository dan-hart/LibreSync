//! Two-device scenarios exercised through the public `Engine` API.
//!
//! These run on every CI platform (Linux and macOS) and cover the three
//! correctness guarantees added in 0.6: field-level merges during sync,
//! delta exchanges, and loss-free append-only op-logs.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use libresync::{
    AppKey, DeviceHandler, DeviceKeys, Engine, EngineConfig, FieldValue, Identity,
    InMemoryLogicalAdapter, LamportClock, MergePolicy, State, SyncRecord,
};

/// Pins one fingerprint per linked device, like a real app allowlist.
struct PinningHandler {
    app_id: String,
    keys: DeviceKeys,
    app_key: AppKey,
    pins: Mutex<HashMap<String, String>>,
}

impl PinningHandler {
    fn pin(&self, device_id: &str, fingerprint: &str) {
        self.pins
            .lock()
            .expect("pins")
            .insert(device_id.to_string(), fingerprint.to_string());
    }
}

impl DeviceHandler for PinningHandler {
    fn app_id(&self) -> &str {
        &self.app_id
    }

    fn is_linked(&self, identity: &Identity) -> bool {
        self.pins
            .lock()
            .expect("pins")
            .contains_key(&identity.device_id)
    }

    fn approve_link(&self, _identity: &Identity) -> libresync::Result<bool> {
        Ok(true)
    }

    fn device_keys(&self) -> libresync::Result<DeviceKeys> {
        Ok(self.keys.clone())
    }

    fn app_key(&self) -> libresync::Result<AppKey> {
        Ok(self.app_key.clone())
    }

    fn is_linked_with_fingerprint(&self, identity: &Identity, fingerprint: &str) -> bool {
        self.pins
            .lock()
            .expect("pins")
            .get(&identity.device_id)
            .map(|pinned| pinned == fingerprint)
            .unwrap_or(false)
    }
}

struct Device {
    engine: Engine,
    adapter: Arc<InMemoryLogicalAdapter>,
    handler: Arc<PinningHandler>,
}

fn device(id: &str, app_key: &AppKey, listen: bool, ops_policy: MergePolicy) -> Device {
    let identity = Identity::new(id, "com.example.twodevice", "user");
    let handler = Arc::new(PinningHandler {
        app_id: identity.app_id.clone(),
        keys: DeviceKeys::generate(&identity).expect("keys"),
        app_key: app_key.clone(),
        pins: Mutex::new(HashMap::new()),
    });
    let mut config = EngineConfig::new(identity);
    if listen {
        config = config.with_listen_addr("127.0.0.1:0".parse::<SocketAddr>().expect("addr"));
    }
    let mut engine = Engine::new(config, State::new(id), handler.clone());
    let adapter = Arc::new(
        InMemoryLogicalAdapter::new("records", "app")
            .with_merge_policy("tags", MergePolicy::SetUnion)
            .with_merge_policy("ops", ops_policy),
    );
    engine
        .register_logical_adapter(adapter.clone())
        .expect("register");
    Device {
        engine,
        adapter,
        handler,
    }
}

fn link(a: &Device, b: &Device) {
    a.handler
        .pin(&b.engine.identity().device_id, b.handler.keys.fingerprint());
    b.handler
        .pin(&a.engine.identity().device_id, a.handler.keys.fingerprint());
}

fn record(
    id: &str,
    counter: u64,
    device: &str,
    fields: BTreeMap<String, FieldValue>,
) -> SyncRecord {
    SyncRecord {
        schema: "s".to_string(),
        entity: "Item".to_string(),
        id: id.to_string(),
        fields,
        clock: LamportClock {
            counter,
            device_id: device.to_string(),
        },
        ..SyncRecord::default()
    }
}

fn s(value: &str) -> FieldValue {
    FieldValue::String(value.to_string())
}

#[test]
fn concurrent_edits_to_different_fields_survive_on_both_devices() {
    let app_key = AppKey::generate().expect("app key");
    let mut a = device("device-a", &app_key, true, MergePolicy::LastWriterWins);
    let b = device("device-b", &app_key, false, MergePolicy::LastWriterWins);
    link(&a, &b);
    let addr = a.engine.start_listening().expect("listen");

    let base = BTreeMap::from([
        ("title".to_string(), s("Buy milk")),
        ("done".to_string(), FieldValue::Bool(false)),
    ]);
    a.adapter
        .upsert_record(record("1", 1, "device-a", base.clone()));
    b.engine.sync_now(addr, "records").expect("initial sync");

    let mut edit_a = base.clone();
    edit_a.insert("title".to_string(), s("Buy oat milk"));
    a.adapter.upsert_record(record("1", 2, "device-a", edit_a));
    let mut edit_b = base.clone();
    edit_b.insert("done".to_string(), FieldValue::Bool(true));
    b.adapter.upsert_record(record("1", 3, "device-b", edit_b));

    b.engine.sync_now(addr, "records").expect("sync");

    for side in [&a, &b] {
        let merged = side.adapter.record("s", "Item", "1").expect("record");
        assert_eq!(merged.fields.get("title"), Some(&s("Buy oat milk")));
        assert_eq!(merged.fields.get("done"), Some(&FieldValue::Bool(true)));
    }
    a.engine.stop_listening().expect("stop");
}

#[test]
fn append_only_event_log_loses_nothing_under_concurrent_appends() {
    let app_key = AppKey::generate().expect("app key");
    let mut a = device("device-a", &app_key, true, MergePolicy::AppendOnly);
    let b = device("device-b", &app_key, false, MergePolicy::AppendOnly);
    link(&a, &b);
    let addr = a.engine.start_listening().expect("listen");

    // One record holds the log; each op is an opaque map with its own id.
    let op = |id: &str, device: &str| {
        FieldValue::Map(BTreeMap::from([
            ("id".to_string(), s(id)),
            ("by".to_string(), s(device)),
        ]))
    };
    a.adapter.upsert_record(record(
        "log",
        1,
        "device-a",
        BTreeMap::from([(
            "ops".to_string(),
            FieldValue::List(vec![op("op-1", "device-a")]),
        )]),
    ));
    b.engine.sync_now(addr, "records").expect("initial sync");

    // Both devices append while offline; B also appends an op that A has
    // and B's copy is *older* than A's newest version.
    for round in 0..3u64 {
        let counter = 2 + round;
        let mut ops_a = match a
            .adapter
            .record("s", "Item", "log")
            .expect("a")
            .fields
            .remove("ops")
        {
            Some(FieldValue::List(items)) => items,
            _ => Vec::new(),
        };
        ops_a.push(op(&format!("a-{round}"), "device-a"));
        a.adapter.upsert_record(record(
            "log",
            counter,
            "device-a",
            BTreeMap::from([("ops".to_string(), FieldValue::List(ops_a))]),
        ));

        let mut ops_b = match b
            .adapter
            .record("s", "Item", "log")
            .expect("b")
            .fields
            .remove("ops")
        {
            Some(FieldValue::List(items)) => items,
            _ => Vec::new(),
        };
        ops_b.push(op(&format!("b-{round}"), "device-b"));
        b.adapter.upsert_record(record(
            "log",
            counter,
            "device-b",
            BTreeMap::from([("ops".to_string(), FieldValue::List(ops_b))]),
        ));
    }

    b.engine.sync_now(addr, "records").expect("sync");
    // A second exchange settles ordering differences without churn.
    let (_, stats) = b
        .engine
        .sync_now_with_stats(addr, "records")
        .expect("settle");
    assert_eq!(stats.entries_received, 0);
    assert_eq!(stats.entries_sent, 0);

    let expected: Vec<String> = ["op-1", "a-0", "b-0", "a-1", "b-1", "a-2", "b-2"]
        .iter()
        .map(|id| id.to_string())
        .collect();
    for side in [&a, &b] {
        let log = side.adapter.record("s", "Item", "log").expect("log");
        let ids: Vec<String> = match log.fields.get("ops") {
            Some(FieldValue::List(items)) => items
                .iter()
                .filter_map(|item| match item {
                    FieldValue::Map(map) => match map.get("id") {
                        Some(FieldValue::String(id)) => Some(id.clone()),
                        _ => None,
                    },
                    _ => None,
                })
                .collect(),
            other => panic!("unexpected ops {other:?}"),
        };
        let mut sorted = ids.clone();
        sorted.sort();
        let mut expected_sorted = expected.clone();
        expected_sorted.sort();
        assert_eq!(
            sorted,
            expected_sorted,
            "{}",
            side.engine.identity().device_id
        );
        assert_eq!(ids.len(), expected.len(), "no duplicates");
    }
    a.engine.stop_listening().expect("stop");
}

#[test]
fn delta_sync_traffic_is_proportional_to_the_edit() {
    let app_key = AppKey::generate().expect("app key");
    let mut a = device("device-a", &app_key, true, MergePolicy::LastWriterWins);
    let b = device("device-b", &app_key, false, MergePolicy::LastWriterWins);
    link(&a, &b);
    let addr = a.engine.start_listening().expect("listen");

    for index in 0..300 {
        a.adapter.upsert_record(record(
            &format!("item-{index}"),
            1,
            "device-a",
            BTreeMap::from([("body".to_string(), s(&"x".repeat(400)))]),
        ));
    }
    let (_, first) = b
        .engine
        .sync_now_with_stats(addr, "records")
        .expect("first");
    assert!(first.full_snapshot);
    assert_eq!(first.entries_received, 300);

    b.adapter.upsert_record(record(
        "item-7",
        2,
        "device-b",
        BTreeMap::from([("body".to_string(), s("edited"))]),
    ));
    let (_, second) = b
        .engine
        .sync_now_with_stats(addr, "records")
        .expect("second");
    assert!(!second.full_snapshot);
    assert_eq!(second.entries_sent, 1);
    assert_eq!(second.entries_received, 0);
    assert!(
        second.bytes_sent < 4_096,
        "sent {} bytes",
        second.bytes_sent
    );
    assert!(
        second.bytes_received < 2_048,
        "received {} bytes",
        second.bytes_received
    );
    assert!(second.bytes_sent * 25 < first.bytes_received);
    assert_eq!(
        a.adapter
            .record("s", "Item", "item-7")
            .expect("record")
            .fields
            .get("body"),
        Some(&s("edited"))
    );
    a.engine.stop_listening().expect("stop");
}
