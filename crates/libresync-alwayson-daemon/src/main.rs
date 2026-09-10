use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use directories::ProjectDirs;
use libresync::{
    register_mdns, AppKey, AutoRefreshConfig, DeviceKeys, Engine, EngineConfig, Event, EventStream,
    Identity, JsonFileAdapter, State,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const FILE_KEY: &str = "file";
const DEFAULT_LISTEN: &str = "0.0.0.0:52345";
const AUTO_APPROVE_INTERVAL_SECS: u64 = 5;
const AUTO_APPROVE_RETRY_SECS: u64 = 300;
const AUTO_APPROVE_DISCOVER_TIMEOUT_SECS: u64 = 2;

fn main() {
    if let Err(error) = run() {
        eprintln!("libresync-alwayson-daemon: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let paths = default_paths()?;
    let config_path = args.config.clone().unwrap_or_else(|| paths.config.clone());
    let data_path = args.data.clone().unwrap_or_else(|| paths.data.clone());

    let (mut config, keys) = load_or_init_config(&config_path, &data_path, &args)?;
    apply_overrides(&mut config, &args, &data_path, &paths.state)?;
    ensure_backup_policy(&mut config, &paths.backups)?;
    save_config(&config_path, &config)?;

    let app_key = config.app_key()?;
    let identity = config.identity();
    let listen_addr = config.listen_addr()?;
    let state = load_or_init_state(&config, &app_key)?;

    let config_arc = Arc::new(Mutex::new(config));
    let handler = Arc::new(AlwaysOnHandler {
        app_id: identity.app_id.clone(),
        keys,
        config: Arc::clone(&config_arc),
        config_path: config_path.clone(),
    });

    let mut engine = Engine::new(
        EngineConfig::new(identity.clone()).with_listen_addr(listen_addr),
        state,
        handler,
    );

    if let Some(data_path) = config_arc
        .lock()
        .map_err(|_| "config lock".to_string())?
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
    let mdns = register_mdns(&identity, listener_addr).map_err(|error| error.to_string())?;

    let auto_refresh = if config_arc
        .lock()
        .map_err(|_| "config lock".to_string())?
        .data_path
        .is_some()
    {
        let fallback_addresses = config_arc
            .lock()
            .map_err(|_| "config lock".to_string())?
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
                    AutoRefreshConfig::new(FILE_KEY, paths.state.clone())
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

    let stream = engine.attach_event_channel();
    spawn_config_listener(stream, Arc::clone(&config_arc), config_path.clone());

    let engine = Arc::new(Mutex::new(engine));
    spawn_auto_approve_loop(
        Arc::clone(&engine),
        Arc::clone(&config_arc),
        config_path.clone(),
    );

    let running = Arc::new(AtomicBool::new(true));
    let running_flag = Arc::clone(&running);
    ctrlc::set_handler(move || {
        running_flag.store(false, Ordering::SeqCst);
    })
    .map_err(|error| error.to_string())?;

    while running.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_secs(1));
    }

    if let Some(auto_refresh) = auto_refresh {
        let _ = auto_refresh.stop();
    }
    let _ = mdns.shutdown();
    if let Ok(mut engine) = engine.lock() {
        let _ = engine.stop_listening();
        if let Ok(state) = engine.state().lock() {
            let _ = state.save_encrypted(&app_key, &paths.state);
        }
    }

    Ok(())
}

#[derive(Debug, Default, Clone)]
struct Args {
    config: Option<PathBuf>,
    data: Option<PathBuf>,
    state: Option<PathBuf>,
    listen: Option<String>,
    device_id: Option<String>,
    app_id: Option<String>,
    user_id: Option<String>,
    auto_accept: bool,
    auto_approve: bool,
    auto_approve_minutes: u64,
    pairing_secret: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    parse_args_from(env::args().skip(1))
}

fn parse_args_from<I>(iter: I) -> Result<Args, String>
where
    I: IntoIterator<Item = String>,
{
    let mut args = Args::default();
    let mut iter = iter.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            "--config" => {
                args.config = Some(next_arg(&mut iter, "--config")?.into());
            }
            "--data" => {
                args.data = Some(next_arg(&mut iter, "--data")?.into());
            }
            "--state" => {
                args.state = Some(next_arg(&mut iter, "--state")?.into());
            }
            "--listen" => {
                args.listen = Some(next_arg(&mut iter, "--listen")?);
            }
            "--device-id" => {
                args.device_id = Some(next_arg(&mut iter, "--device-id")?);
            }
            "--app-id" => {
                args.app_id = Some(next_arg(&mut iter, "--app-id")?);
            }
            "--user-id" => {
                args.user_id = Some(next_arg(&mut iter, "--user-id")?);
            }
            "--auto-accept" => {
                args.auto_accept = true;
            }
            "--auto-approve" => {
                args.auto_approve = true;
            }
            "--auto-approve-minutes" => {
                let value = next_arg(&mut iter, "--auto-approve-minutes")?;
                args.auto_approve_minutes = value
                    .parse::<u64>()
                    .map_err(|_| "invalid auto-approve minutes".to_string())?;
            }
            "--pairing-secret" => {
                args.pairing_secret = Some(next_arg(&mut iter, "--pairing-secret")?);
            }
            value => return Err(format!("unknown argument: {value}")),
        }
    }

    if args.auto_approve_minutes == 0 {
        args.auto_approve_minutes = 15;
    }

    Ok(args)
}

fn next_arg(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    iter.next()
        .ok_or_else(|| format!("missing value for {flag}"))
}

fn print_usage() {
    println!("LibreSyncAlwaysOn daemon\n");
    println!("USAGE:\n  libresync-alwayson-daemon [options]\n");
    println!("OPTIONS:");
    println!("  --config <path>       Path to config.json (default: app data dir)");
    println!("  --data <path>         JSON data file to sync (default: app data dir)");
    println!("  --state <path>        Encrypted state file (default: app data dir)");
    println!("  --listen <addr>       Listen address (default: {DEFAULT_LISTEN})");
    println!("  --device-id <id>      Device ID for first-run init");
    println!("  --app-id <id>         App ID for first-run init");
    println!("  --user-id <id>        User ID for first-run init");
    println!("  --auto-accept         Auto-accept linking requests");
    println!("  --auto-approve        Auto-approve links on private LANs");
    println!("  --auto-approve-minutes <n>  Auto-approve duration in minutes (default: 15)");
    println!("  --pairing-secret <value>    Require this pairing secret for linking");
    println!("  -h, --help            Show this help message");
}

#[derive(Clone)]
struct AlwaysOnPaths {
    config: PathBuf,
    backups: PathBuf,
    data: PathBuf,
    state: PathBuf,
}

fn default_paths() -> Result<AlwaysOnPaths, String> {
    let dirs = ProjectDirs::from("com", "codedbydan", "LibreSyncAlwaysOn")
        .ok_or_else(|| "unable to resolve app data dir".to_string())?;
    let base = dirs.data_dir();
    fs::create_dir_all(base).map_err(|error| error.to_string())?;
    Ok(AlwaysOnPaths {
        config: base.join("config.json"),
        backups: base.join("backups"),
        data: base.join("libresync.json"),
        state: base.join("state.json"),
    })
}

fn apply_overrides(
    config: &mut AlwaysOnConfig,
    args: &Args,
    data_path: &Path,
    state_path: &Path,
) -> Result<(), String> {
    if let Some(listen) = args.listen.as_ref() {
        config.listen_addr = listen.clone();
    }
    if args.auto_accept {
        config.auto_accept_linking = true;
    }
    if args.auto_approve {
        config.auto_approve_linking = true;
        config.auto_accept_linking = true;
        config.auto_approve_until =
            Some(now_unix_secs().saturating_add(args.auto_approve_minutes.saturating_mul(60)));
    }
    if let Some(secret) = args.pairing_secret.clone() {
        if !secret.trim().is_empty() {
            config.pairing_secret = Some(secret);
        }
    }
    if args.data.is_some() {
        config.data_path = Some(data_path.to_path_buf());
    }
    if args.state.is_some() {
        config.state_path = state_path.to_path_buf();
    }
    Ok(())
}

fn load_or_init_config(
    path: &Path,
    data_path: &Path,
    args: &Args,
) -> Result<(AlwaysOnConfig, DeviceKeys), String> {
    if path.exists() {
        let data = fs::read(path).map_err(|error| error.to_string())?;
        let config: AlwaysOnConfig =
            serde_json::from_slice(&data).map_err(|error| error.to_string())?;
        let keys = config.device_keys()?;
        return Ok((config, keys));
    }

    let user_id = args.user_id.clone().unwrap_or_else(whoami::username);
    let device_id = args
        .device_id
        .clone()
        .unwrap_or_else(|| format!("always-on-{}", Uuid::new_v4().simple()));
    let app_id = args
        .app_id
        .clone()
        .unwrap_or_else(|| "com.codedbydan.libresync".to_string());
    let identity = Identity::new(&device_id, &app_id, &user_id);
    let keys = DeviceKeys::generate(&identity).map_err(|error| error.to_string())?;
    let app_key = AppKey::generate().map_err(|error| error.to_string())?;

    let config = AlwaysOnConfig {
        app_id,
        device_id,
        user_id,
        listen_addr: args
            .listen
            .clone()
            .unwrap_or_else(|| DEFAULT_LISTEN.to_string()),
        state_path: args
            .state
            .clone()
            .unwrap_or_else(|| data_path.with_file_name("state.json")),
        data_path: Some(data_path.to_path_buf()),
        device_keys: DeviceKeysRecord::from_keys(&keys),
        app_key: BASE64.encode(app_key.as_bytes()),
        devices: BTreeMap::new(),
        auto_accept_linking: args.auto_accept,
        auto_approve_linking: args.auto_approve,
        auto_approve_until: if args.auto_approve {
            Some(now_unix_secs() + args.auto_approve_minutes.saturating_mul(60))
        } else {
            None
        },
        pairing_secret: args
            .pairing_secret
            .clone()
            .filter(|value| !value.trim().is_empty()),
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
    let entry = config.backup.entry(config.app_id.clone()).or_default();
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

fn spawn_config_listener(
    stream: EventStream,
    config: Arc<Mutex<AlwaysOnConfig>>,
    config_path: PathBuf,
) {
    thread::spawn(move || {
        while let Ok(event) = stream.recv() {
            let mut changed = false;
            match event {
                Event::DeviceSeen { device } | Event::SyncFinished { device, .. } => {
                    if let Ok(mut config) = config.lock() {
                        config.upsert_device(&device.identity, device.address, device.fingerprint);
                        changed = true;
                    }
                }
                Event::LinkingRequested { request }
                | Event::LinkingDecisionRequired { request } => {
                    if let Ok(mut config) = config.lock() {
                        config.upsert_device(
                            &request.device.identity,
                            request.device.address,
                            request.device.fingerprint,
                        );
                        changed = true;
                    }
                }
                Event::FingerprintChanged {
                    device,
                    fingerprint,
                } => {
                    // Never re-pin automatically: the stored fingerprint stays
                    // and the connection was rejected. A person must re-link.
                    eprintln!(
                        "WARNING: device {} presented a different certificate (fingerprint {}); \
                         connection rejected. Re-link the device if this change is expected.",
                        device.identity.device_id, fingerprint
                    );
                }
                _ => {}
            }

            if changed {
                if let Ok(config) = config.lock() {
                    let _ = save_config(&config_path, &config);
                }
            }
        }
    });
}

fn spawn_auto_approve_loop(
    engine: Arc<Mutex<Engine>>,
    config: Arc<Mutex<AlwaysOnConfig>>,
    config_path: PathBuf,
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
                        eprintln!("Auto-approve discovery error: {error}");
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
                        println!(
                            "Auto-approved link with {} ({})",
                            remote.identity.device_id, remote.identity.user_id
                        );
                    }
                    Err(error) => {
                        attempts.insert(device_id, now);
                        eprintln!("Auto-approve link failed: {error}");
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

    fn approve_link(&self, identity: &libresync::Identity) -> libresync::Result<bool> {
        let auto_accept = {
            let config = self.config.lock().expect("config lock");
            config.auto_accept_linking
        };
        if auto_accept {
            if let Ok(mut config) = self.config.lock() {
                config.upsert_device(identity, None, None);
                let _ = save_config(&self.config_path, &config);
            }
        }
        Ok(auto_accept)
    }

    fn device_keys(&self) -> libresync::Result<DeviceKeys> {
        Ok(self.keys.clone())
    }

    fn app_key(&self) -> libresync::Result<AppKey> {
        let config = self.config.lock().expect("config lock");
        config.app_key().map_err(libresync::Error::Protocol)
    }

    fn set_app_key(&self, app_key: &AppKey) -> libresync::Result<()> {
        let mut config = self.config.lock().expect("config lock");
        config.set_app_key(app_key);
        save_config(&self.config_path, &config).map_err(libresync::Error::Protocol)
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

    fn approve_link_with_fingerprint(
        &self,
        identity: &libresync::Identity,
        fingerprint: &str,
    ) -> libresync::Result<bool> {
        let auto_accept = {
            let config = self.config.lock().expect("config lock");
            config.auto_accept_linking
        };
        if auto_accept {
            if let Ok(mut config) = self.config.lock() {
                config.upsert_device(identity, None, Some(fingerprint.to_string()));
                let _ = save_config(&self.config_path, &config);
            }
        }
        Ok(auto_accept)
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

#[cfg(test)]
mod tests {
    use super::*;
    use libresync::DeviceHandler;
    use std::fs;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::thread;
    use std::time::Duration;
    use tempfile::tempdir;

    fn base_config(tempdir: &Path) -> AlwaysOnConfig {
        let identity = Identity::new("device-a", "app-a", "user-a");
        let keys = DeviceKeys::generate(&identity).expect("device keys");
        let app_key = AppKey::generate().expect("app key");
        AlwaysOnConfig {
            app_id: identity.app_id.clone(),
            device_id: identity.device_id.clone(),
            user_id: identity.user_id.clone(),
            listen_addr: DEFAULT_LISTEN.to_string(),
            state_path: tempdir.join("state.json"),
            data_path: Some(tempdir.join("data.json")),
            device_keys: DeviceKeysRecord::from_keys(&keys),
            app_key: BASE64.encode(app_key.as_bytes()),
            devices: BTreeMap::new(),
            auto_accept_linking: false,
            auto_approve_linking: false,
            auto_approve_until: None,
            pairing_secret: None,
            backup: BTreeMap::new(),
        }
    }

    #[test]
    fn parse_args_from_sets_defaults() {
        let args = parse_args_from(vec!["--auto-approve".to_string()]).expect("args");
        assert!(args.auto_approve);
        assert_eq!(args.auto_approve_minutes, 15);
    }

    #[test]
    fn parse_args_from_reads_values() {
        let args = parse_args_from(vec![
            "--config".to_string(),
            "cfg.json".to_string(),
            "--data".to_string(),
            "data.json".to_string(),
            "--state".to_string(),
            "state.json".to_string(),
            "--listen".to_string(),
            "127.0.0.1:1".to_string(),
            "--device-id".to_string(),
            "device".to_string(),
            "--app-id".to_string(),
            "app".to_string(),
            "--user-id".to_string(),
            "user".to_string(),
            "--auto-accept".to_string(),
            "--auto-approve".to_string(),
            "--auto-approve-minutes".to_string(),
            "5".to_string(),
            "--pairing-secret".to_string(),
            "secret".to_string(),
        ])
        .expect("args");
        assert_eq!(args.config.as_deref(), Some(Path::new("cfg.json")));
        assert_eq!(args.data.as_deref(), Some(Path::new("data.json")));
        assert_eq!(args.state.as_deref(), Some(Path::new("state.json")));
        assert_eq!(args.listen.as_deref(), Some("127.0.0.1:1"));
        assert_eq!(args.device_id.as_deref(), Some("device"));
        assert_eq!(args.app_id.as_deref(), Some("app"));
        assert_eq!(args.user_id.as_deref(), Some("user"));
        assert!(args.auto_accept);
        assert!(args.auto_approve);
        assert_eq!(args.auto_approve_minutes, 5);
        assert_eq!(args.pairing_secret.as_deref(), Some("secret"));
    }

    #[test]
    fn apply_overrides_sets_auto_approve_and_pairing_secret() {
        let tempdir = tempdir().expect("tempdir");
        let data_path = tempdir.path().join("data.json");
        let state_path = tempdir.path().join("state.json");
        let mut config = base_config(tempdir.path());
        let args = Args {
            listen: Some("127.0.0.1:9999".to_string()),
            auto_accept: false,
            auto_approve: true,
            auto_approve_minutes: 10,
            pairing_secret: Some(" secret ".to_string()),
            data: Some(data_path.clone()),
            state: Some(state_path.clone()),
            ..Args::default()
        };

        apply_overrides(&mut config, &args, &data_path, &state_path).expect("apply overrides");
        assert_eq!(config.listen_addr, "127.0.0.1:9999");
        assert!(config.auto_accept_linking);
        assert!(config.auto_approve_linking);
        assert!(config.auto_approve_until.is_some());
        assert_eq!(config.pairing_secret.as_deref(), Some(" secret "));
        assert_eq!(config.data_path.as_deref(), Some(data_path.as_path()));
        assert_eq!(config.state_path, state_path);
    }

    #[test]
    fn apply_overrides_ignores_blank_pairing_secret() {
        let tempdir = tempdir().expect("tempdir");
        let data_path = tempdir.path().join("data.json");
        let state_path = tempdir.path().join("state.json");
        let mut config = base_config(tempdir.path());
        let args = Args {
            pairing_secret: Some("   ".to_string()),
            ..Args::default()
        };
        apply_overrides(&mut config, &args, &data_path, &state_path).expect("apply overrides");
        assert!(config.pairing_secret.is_none());
    }

    #[test]
    fn load_or_init_config_round_trip() {
        let tempdir = tempdir().expect("tempdir");
        let config_path = tempdir.path().join("config.json");
        let data_path = tempdir.path().join("data.json");
        let args = Args {
            device_id: Some("device-x".to_string()),
            app_id: Some("app-x".to_string()),
            user_id: Some("user-x".to_string()),
            ..Args::default()
        };

        let (config, keys) =
            load_or_init_config(&config_path, &data_path, &args).expect("init config");
        assert_eq!(config.device_id, "device-x");
        assert_eq!(config.app_id, "app-x");
        assert_eq!(config.user_id, "user-x");
        assert!(config_path.exists());
        assert_eq!(keys.fingerprint(), config.device_keys.fingerprint);

        let (loaded, _) =
            load_or_init_config(&config_path, &data_path, &Args::default()).expect("load config");
        assert_eq!(loaded.device_id, "device-x");
    }

    #[test]
    fn auto_approve_state_expires() {
        let tempdir = tempdir().expect("tempdir");
        let mut config = base_config(tempdir.path());
        config.auto_approve_linking = true;
        config.auto_approve_until = Some(now_unix_secs().saturating_sub(1));

        let (active, expired) = config.auto_approve_state();
        assert!(!active);
        assert!(expired);
        assert!(!config.auto_approve_linking);
        assert!(config.auto_approve_until.is_none());
    }

    #[test]
    fn auto_approve_state_active_without_expiry() {
        let tempdir = tempdir().expect("tempdir");
        let mut config = base_config(tempdir.path());
        config.auto_approve_linking = true;
        config.auto_approve_until = None;

        let (active, expired) = config.auto_approve_state();
        assert!(active);
        assert!(!expired);
    }

    #[test]
    fn auto_approve_address_filters_networks() {
        let private = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5)), 1234);
        let link_local = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(169, 254, 1, 2)), 1234);
        let public = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 1234);
        let unique_local =
            SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1)), 1234);
        let v6_link_local =
            SocketAddr::new(IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)), 1234);

        assert!(is_auto_approve_address(private));
        assert!(is_auto_approve_address(link_local));
        assert!(!is_auto_approve_address(public));
        assert!(is_auto_approve_address(unique_local));
        assert!(is_auto_approve_address(v6_link_local));
    }

    #[test]
    fn config_helpers_round_trip() {
        let tempdir = tempdir().expect("tempdir");
        let mut config = base_config(tempdir.path());

        let parsed = config.listen_addr().expect("listen addr");
        assert_eq!(parsed, DEFAULT_LISTEN.parse::<SocketAddr>().unwrap());

        let app_key = config.app_key().expect("app key");
        let rotated = AppKey::generate().expect("app key");
        config.set_app_key(&rotated);
        let updated = config.app_key().expect("app key");
        assert_ne!(app_key.as_bytes(), updated.as_bytes());

        let keys = config.device_keys().expect("device keys");
        assert_eq!(keys.fingerprint(), config.device_keys.fingerprint);

        let identity = Identity::new("remote", "app-a", "user-b");
        let addr = "127.0.0.1:5555".parse::<SocketAddr>().unwrap();
        config.upsert_device(&identity, Some(addr), Some("fp".to_string()));
        let record = config.devices.get("remote").expect("record");
        assert_eq!(record.last_seen_addr.as_deref(), Some("127.0.0.1:5555"));
        assert_eq!(record.fingerprint.as_deref(), Some("fp"));
    }

    #[test]
    fn ensure_backup_policy_sets_defaults() {
        let tempdir = tempdir().expect("tempdir");
        let mut config = base_config(tempdir.path());
        let backups = tempdir.path().join("backups");

        ensure_backup_policy(&mut config, &backups).expect("backup policy");
        let policy = config.backup.get("app-a").expect("policy");
        assert_eq!(policy.backup_dir.as_deref(), Some(backups.as_path()));
        assert_eq!(policy.max_snapshots, Some(20));
        assert_eq!(policy.max_age_days, Some(30));
    }

    #[test]
    fn load_or_init_state_round_trip_and_mismatch() {
        let tempdir = tempdir().expect("tempdir");
        let mut config = base_config(tempdir.path());
        let app_key = config.app_key().expect("app key");

        let state = load_or_init_state(&config, &app_key).expect("state");
        assert_eq!(state.device_id, config.device_id);
        assert!(config.state_path.exists());

        let state = load_or_init_state(&config, &app_key).expect("state");
        assert_eq!(state.device_id, config.device_id);

        config.device_id = "other-device".to_string();
        let error = load_or_init_state(&config, &app_key).expect_err("mismatch");
        assert!(error.contains("state device id does not match config"));
    }

    #[test]
    fn handler_updates_config_and_persists() {
        let tempdir = tempdir().expect("tempdir");
        let config_path = tempdir.path().join("config.json");
        let mut config = base_config(tempdir.path());
        config.auto_accept_linking = true;
        config.pairing_secret = Some("secret".to_string());
        save_config(&config_path, &config).expect("save");

        let config_arc = Arc::new(Mutex::new(config));
        let keys = config_arc
            .lock()
            .expect("lock")
            .device_keys()
            .expect("keys");
        let handler = AlwaysOnHandler {
            app_id: "app-a".to_string(),
            keys,
            config: Arc::clone(&config_arc),
            config_path: config_path.clone(),
        };

        let identity = Identity::new("remote", "app-a", "user-x");
        assert!(handler.approve_link(&identity).expect("approve"));
        assert!(handler.is_linked(&identity));

        assert!(handler
            .approve_link_with_fingerprint(&identity, "fp")
            .expect("approve"));
        assert!(handler.is_linked_with_fingerprint(&identity, "fp"));

        assert_eq!(handler.pairing_secret().as_deref(), Some("secret"));

        let rotated = AppKey::generate().expect("app key");
        handler.set_app_key(&rotated).expect("set app key");
        let reloaded = {
            let bytes = fs::read(&config_path).expect("read config");
            serde_json::from_slice::<AlwaysOnConfig>(&bytes).expect("config")
        };
        let updated = reloaded.app_key().expect("app key");
        assert_eq!(updated.as_bytes(), rotated.as_bytes());
    }

    #[test]
    fn spawn_config_listener_records_events() {
        let tempdir = tempdir().expect("tempdir");
        let config_path = tempdir.path().join("config.json");
        let config = base_config(tempdir.path());
        save_config(&config_path, &config).expect("save config");

        let config_arc = Arc::new(Mutex::new(config));
        let (sink, stream) = libresync::event_channel();
        spawn_config_listener(stream, Arc::clone(&config_arc), config_path.clone());

        let identity = Identity::new("remote-device", "app-a", "user-a");
        let addr = "127.0.0.1:1234".parse::<SocketAddr>().unwrap();
        let device = libresync::DeviceInfo {
            identity: identity.clone(),
            address: Some(addr),
            last_seen: None,
            linked: false,
            fingerprint: Some("fp".to_string()),
        };
        sink.emit(Event::DeviceSeen {
            device: device.clone(),
        });
        sink.emit(Event::LinkingRequested {
            request: libresync::LinkingRequest { device, code: None },
        });

        drop(sink);
        thread::sleep(Duration::from_millis(50));

        let loaded = fs::read(&config_path).expect("read config");
        let loaded: AlwaysOnConfig = serde_json::from_slice(&loaded).expect("parse config");
        let record = loaded.devices.get("remote-device").expect("record");
        assert_eq!(record.last_seen_addr.as_deref(), Some("127.0.0.1:1234"));
        assert_eq!(record.fingerprint.as_deref(), Some("fp"));
    }
}
