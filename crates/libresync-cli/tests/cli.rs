use std::collections::HashSet;
use std::fs;
use std::sync::{Arc, Mutex};

use assert_cmd::cargo::cargo_bin_cmd;
use libresync::{DeviceHandler, Identity, Result, State, SyncListener};
use predicates::str::contains;

const APP_ID: &str = "com.codedbydan.libresync-cli";
const FILE_KEY: &str = "file";

struct TestHandler {
    app_id: String,
    paired: Mutex<HashSet<String>>,
}

impl TestHandler {
    fn new(app_id: &str) -> Self {
        Self {
            app_id: app_id.to_string(),
            paired: Mutex::new(HashSet::new()),
        }
    }
}

impl DeviceHandler for TestHandler {
    fn app_id(&self) -> &str {
        &self.app_id
    }

    fn is_paired(&self, identity: &Identity) -> bool {
        self.paired
            .lock()
            .expect("paired lock")
            .contains(&identity.device_id)
    }

    fn approve_pair(&self, identity: &Identity) -> Result<bool> {
        self.paired
            .lock()
            .expect("paired lock")
            .insert(identity.device_id.clone());
        Ok(true)
    }
}

#[test]
fn cli_init_and_select_sets_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let config_path = temp.path().join("config.json");
    let data_path = temp.path().join("data.json");

    cargo_bin_cmd!("libresync")
        .args([
            "init",
            "--config",
            config_path.to_str().unwrap(),
            "--device-id",
            "brisk-river-summit",
            "--user-id",
            "calm-forest",
        ])
        .assert()
        .success();

    cargo_bin_cmd!("libresync")
        .args([
            "select",
            "--config",
            config_path.to_str().unwrap(),
            "--file",
            data_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let config_raw = fs::read_to_string(&config_path).expect("read config");
    assert!(config_raw.contains(APP_ID));
    assert!(config_raw.contains("brisk-river-summit"));
    assert!(data_path.exists());
}

#[test]
fn cli_pair_and_refresh_updates_file() {
    let handler = Arc::new(TestHandler::new(APP_ID));
    let listener_state = Arc::new(Mutex::new(State::new("listener-device")));
    {
        let mut state = listener_state.lock().expect("state");
        state.set(FILE_KEY.to_string(), br#"{"listener":true}"#.to_vec());
        state.set(FILE_KEY.to_string(), br#"{"listener":true,"v":2}"#.to_vec());
    }

    let listener_identity = Identity::new("listener-device", APP_ID, "listener-user");
    let listener = SyncListener::start(
        "127.0.0.1:0".parse().expect("addr"),
        listener_identity,
        listener_state.clone(),
        handler,
    )
    .expect("listener start");

    let temp = tempfile::tempdir().expect("tempdir");
    let config_path = temp.path().join("device.json");
    let data_path = temp.path().join("data.json");

    cargo_bin_cmd!("libresync")
        .args([
            "init",
            "--config",
            config_path.to_str().unwrap(),
            "--device-id",
            "device-id",
            "--user-id",
            "brisk-lake",
        ])
        .assert()
        .success();

    cargo_bin_cmd!("libresync")
        .args([
            "select",
            "--config",
            config_path.to_str().unwrap(),
            "--file",
            data_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    fs::write(&data_path, br#"{"device":true}"#).expect("write data");

    cargo_bin_cmd!("libresync")
        .args([
            "pair",
            "--config",
            config_path.to_str().unwrap(),
            "--device",
            &listener.addr().to_string(),
            "--yes",
        ])
        .assert()
        .success();

    cargo_bin_cmd!("libresync")
        .args([
            "refresh",
            "--config",
            config_path.to_str().unwrap(),
            "--device",
            &listener.addr().to_string(),
        ])
        .assert()
        .success();

    let updated = fs::read_to_string(&data_path).expect("read data");
    assert!(updated.contains("\"listener\""));

    let listener_snapshot = listener_state.lock().expect("state").snapshot();
    assert!(listener_snapshot.iter().any(|entry| entry.key == FILE_KEY));

    listener.shutdown().expect("shutdown");
}

#[test]
fn cli_help_mentions_commands() {
    cargo_bin_cmd!("libresync")
        .args(["--help"])
        .assert()
        .success()
        .stdout(contains("init"))
        .stdout(contains("discover"))
        .stdout(contains("pair"))
        .stdout(contains("listen"))
        .stdout(contains("watch"))
        .stdout(contains("stop"))
        .stdout(contains("status"))
        .stdout(contains("refresh"));
}

#[test]
fn cli_status_runs_without_discovery() {
    let temp = tempfile::tempdir().expect("tempdir");
    let config_path = temp.path().join("config.json");

    cargo_bin_cmd!("libresync")
        .args([
            "init",
            "--config",
            config_path.to_str().unwrap(),
            "--device-id",
            "signal-river-vista",
            "--user-id",
            "steady-forest",
        ])
        .assert()
        .success();

    cargo_bin_cmd!("libresync")
        .args([
            "status",
            "--config",
            config_path.to_str().unwrap(),
            "--no-discover",
        ])
        .assert()
        .success()
        .stdout(contains("Device ID"))
        .stdout(contains("Paired devices"));
}
