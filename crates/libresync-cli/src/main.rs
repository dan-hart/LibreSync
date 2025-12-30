use std::collections::{BTreeMap, HashMap};
use std::error::Error as StdError;
use std::fs;
use std::io::{self, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{Local, TimeZone};
use clap::{Parser, Subcommand};
use directories::ProjectDirs;
use libresync::{
    DataAdapter, DeviceHandler, DeviceInfo, DeviceKeys, Engine, EngineConfig, Identity,
    JsonFileAdapter, State,
};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, System};

const APP_ID_DEFAULT: &str = "com.codedbydan.libresync-cli";
const FILE_KEY: &str = "file";
const DEFAULT_LISTEN: &str = "0.0.0.0:52345";
const CONFIG_FILE_NAME: &str = "libresync.json";
const CONFIG_QUALIFIER: &str = "com";
const CONFIG_ORG: &str = "codedbydan";
const CONFIG_APP: &str = "libresync-cli";

fn default_config_path() -> PathBuf {
    ProjectDirs::from(CONFIG_QUALIFIER, CONFIG_ORG, CONFIG_APP)
        .map(|dirs| dirs.config_dir().join(CONFIG_FILE_NAME))
        .unwrap_or_else(|| PathBuf::from(CONFIG_FILE_NAME))
}

fn resolve_config_path(config: Option<PathBuf>) -> PathBuf {
    config.unwrap_or_else(default_config_path)
}

fn log_path_for_config(config_path: &Path) -> PathBuf {
    let dir = config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    dir.join("libresync-listen.log")
}

fn pid_path_for_config(config_path: &Path) -> PathBuf {
    let dir = config_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    dir.join("libresync-listen.pid")
}

#[derive(Parser)]
#[command(
    name = "libresync",
    version,
    about = "Local-only device-to-device refresh CLI for LibreSync.",
    long_about = "A minimal CLI for exercising the LibreSync core. Use it to discover devices on LAN,\npair devices, and refresh a selected JSON file over direct connections."
)]
struct Cli {
    #[arg(
        short,
        long,
        global = true,
        help = "Enable verbose error output.",
        long_help = "Enable verbose error output, including debug formatting and error causes."
    )]
    verbose: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(
        about = "Create a new LibreSync config with device/app/user identity.",
        long_about = "Creates a config file that stores the device ID, user ID, app ID, allowlisted devices, and internal state paths. This is required before discovery, pairing, or refresh. Use --force to overwrite an existing config."
    )]
    Init {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory).",
            long_help = "Path where the LibreSync config JSON will be created. The config stores identity and trust state. Defaults to the OS config directory when omitted."
        )]
        config: Option<PathBuf>,
        #[arg(
            long,
            default_value = APP_ID_DEFAULT,
            help = "App bundle identifier used to scope discovery and trust.",
            long_help = "App bundle identifier used to scope discovery and trust. Devices only pair and refresh when app IDs match."
        )]
        app_id: String,
        #[arg(
            long,
            help = "Device ID override (three words separated by dashes).",
            long_help = "Override the generated device ID. Use three words separated by dashes (e.g. amber-river-summit)."
        )]
        device_id: Option<String>,
        #[arg(
            long,
            help = "User ID override (two words separated by a dash).",
            long_help = "Override the generated user ID. Use an adjective-noun pair separated by a dash (e.g. calm-forest)."
        )]
        user_id: Option<String>,
        #[arg(
            long,
            help = "Overwrite the config file if it already exists.",
            long_help = "Overwrite an existing config file. Use with care; existing trust state will be replaced."
        )]
        force: bool,
    },
    #[command(
        about = "Select the JSON file to keep refreshed.",
        long_about = "Sets the JSON file path that LibreSync will refresh. The file is created if it does not exist."
    )]
    Select {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory).",
            long_help = "Config file that stores the selected JSON file path. Defaults to the OS config directory when omitted."
        )]
        config: Option<PathBuf>,
        #[arg(
            long,
            help = "Path to the JSON file to refresh.",
            long_help = "Path to the JSON file that will be refreshed between devices. The file is created if missing."
        )]
        file: PathBuf,
    },
    #[command(
        about = "Discover devices on the LAN running the same app ID.",
        long_about = "List devices discovered via mDNS for the current app ID. Discovery is unauthenticated and does not grant trust."
    )]
    Discover {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory).",
            long_help = "Config file used to determine the app ID and discovery scope. Defaults to the OS config directory when omitted."
        )]
        config: Option<PathBuf>,
        #[arg(
            long,
            default_value_t = 3,
            help = "Duration in seconds to listen for mDNS responses.",
            long_help = "Time to wait for LAN discovery responses. Increase this value on slower networks."
        )]
        timeout_secs: u64,
    },
    #[command(
        about = "Pair with a device by address (requires device consent).",
        long_about = "Pair establishes trust only. It does not refresh any data. The remote device must accept the pairing request, and you must confirm locally unless --yes is set."
    )]
    Pair {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory).",
            long_help = "Config file to store the paired device entry and trust state. Defaults to the OS config directory when omitted."
        )]
        config: Option<PathBuf>,
        #[arg(
            long,
            help = "Device address to connect to (e.g. 192.168.1.10:52345).",
            long_help = "Socket address for the device listener you want to pair with. If omitted, LibreSync will try to discover devices on the LAN."
        )]
        device: Option<SocketAddr>,
        #[arg(
            long,
            conflicts_with = "device",
            help = "Device ID to pair with (uses discovery).",
            long_help = "Device ID to pair with. LibreSync will discover devices on the LAN and connect to the matching device ID."
        )]
        device_id: Option<String>,
        #[arg(
            long,
            help = "Auto-accept the local pairing prompt.",
            long_help = "Skip the local confirmation prompt after the remote device accepts."
        )]
        yes: bool,
    },
    #[command(
        about = "Run a device listener and advertise on LAN.",
        long_about = "Start a device listener that accepts inbound connections and (by default) advertises via mDNS. The listener runs in the background by default; use --foreground to keep it attached to your terminal."
    )]
    Listen {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory).",
            long_help = "Config file that contains the device identity and trust state. Defaults to the OS config directory when omitted."
        )]
        config: Option<PathBuf>,
        #[arg(
            long,
            default_value = DEFAULT_LISTEN,
            help = "Address to listen on (e.g. 0.0.0.0:52345).",
            long_help = "Socket address to bind for inbound connections. Use 0.0.0.0 to listen on all interfaces."
        )]
        listen: SocketAddr,
        #[arg(
            long,
            help = "Auto-accept incoming pairing requests.",
            long_help = "Automatically approve pairing requests from devices with the same app ID."
        )]
        auto_accept: bool,
        #[arg(
            long,
            help = "Hide the mDNS advertisement (no LAN discovery).",
            long_help = "Disable mDNS advertising. The listener still accepts direct connections by address."
        )]
        no_discovery: bool,
        #[arg(
            long,
            help = "Run the listener in the foreground.",
            long_help = "Run the listener in the foreground instead of spawning a background process. Use this when you want to see logs in the terminal."
        )]
        foreground: bool,
        #[arg(long, hide = true)]
        duration_secs: Option<u64>,
    },
    #[command(
        name = "refresh",
        alias = "sync",
        about = "Refresh the selected JSON file with a paired device.",
        long_about = "Refresh exchanges data only after trust is established. It requires prior pairing. The file is loaded into the local state before refresh and written back after refresh."
    )]
    Refresh {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory).",
            long_help = "Config file that includes the selected JSON file path and device allowlist. Defaults to the OS config directory when omitted."
        )]
        config: Option<PathBuf>,
        #[arg(
            long,
            help = "Device address to connect to (e.g. 192.168.1.10:52345).",
            long_help = "Socket address for the device listener you want to refresh with. If omitted, LibreSync will try to discover devices on the LAN."
        )]
        device: Option<SocketAddr>,
        #[arg(
            long,
            conflicts_with = "device",
            help = "Device ID to refresh with (uses discovery).",
            long_help = "Device ID to refresh with. LibreSync will discover devices on the LAN and connect to the matching device ID."
        )]
        device_id: Option<String>,
    },
    #[command(
        about = "Watch the selected JSON file and refresh all paired devices.",
        long_about = "Watch the selected JSON file for local changes and refresh with all paired devices. Refresh pulls and pushes data, providing best-effort bidirectional updates."
    )]
    Watch {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory).",
            long_help = "Config file that includes the selected JSON file path and device allowlist. Defaults to the OS config directory when omitted."
        )]
        config: Option<PathBuf>,
        #[arg(
            long,
            default_value = DEFAULT_LISTEN,
            help = "Local address to listen on while watching.",
            long_help = "Local address to listen on while watching. Defaults to 0.0.0.0:52345."
        )]
        listen: SocketAddr,
        #[arg(
            long,
            default_value_t = 1,
            help = "Seconds between background refresh cycles.",
            long_help = "Interval in seconds to refresh with paired devices even if no local change is detected."
        )]
        interval_secs: u64,
        #[arg(
            long,
            default_value_t = 200,
            help = "Debounce window in milliseconds for local file events.",
            long_help = "Debounce window in milliseconds for local file events to avoid repeated refresh bursts."
        )]
        debounce_ms: u64,
        #[arg(
            long,
            help = "Disable LAN discovery when resolving paired devices.",
            long_help = "Skip LAN discovery and only use the last seen addresses stored in the config."
        )]
        no_discover: bool,
        #[arg(
            long,
            help = "Disable auto-starting a local listener while watching.",
            long_help = "Skip auto-starting a local listener while watching. Use this if you already run `libresync listen`."
        )]
        no_listen: bool,
    },
    #[command(
        about = "Stop the background listener for this config.",
        long_about = "Stop the background listener spawned by `libresync listen`. This reads the PID from the config directory and terminates the process."
    )]
    Stop {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory).",
            long_help = "Config file used to find the background listener PID. Defaults to the OS config directory when omitted."
        )]
        config: Option<PathBuf>,
    },
    #[command(
        about = "Show device status, paired devices, and recent discovery info.",
        long_about = "Show local identity, selected file, paired devices, and (by default) devices discovered on the LAN. Discovered devices are treated as connected now. Use --no-discover to skip LAN discovery."
    )]
    Status {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory).",
            long_help = "Config file that includes identity, selected file, and paired devices. Defaults to the OS config directory when omitted."
        )]
        config: Option<PathBuf>,
        #[arg(
            long,
            help = "Disable LAN discovery for this status check.",
            long_help = "Skip mDNS discovery and only show stored paired device information."
        )]
        no_discover: bool,
        #[arg(
            long,
            default_value_t = 3,
            help = "Duration in seconds to listen for mDNS responses.",
            long_help = "Time to wait for LAN discovery responses when status discovery is enabled."
        )]
        timeout_secs: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Config {
    device_id: String,
    app_id: String,
    user_id: String,
    state_path: PathBuf,
    data_path: Option<PathBuf>,
    #[serde(default)]
    device_keys: Option<DeviceKeysRecord>,
    #[serde(default)]
    devices: BTreeMap<String, DeviceRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceKeysRecord {
    cert_der: String,
    key_der: String,
    fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceRecord {
    device_id: String,
    user_id: String,
    app_id: String,
    last_seen_addr: Option<String>,
    #[serde(default)]
    last_seen_unix_secs: Option<u64>,
    #[serde(default)]
    fingerprint: Option<String>,
}

impl DeviceKeysRecord {
    fn from_keys(keys: &DeviceKeys) -> Self {
        Self {
            cert_der: BASE64.encode(keys.cert_der()),
            key_der: BASE64.encode(keys.key_der()),
            fingerprint: keys.fingerprint().to_string(),
        }
    }
}

impl Config {
    fn identity(&self) -> Identity {
        Identity::new(&self.device_id, &self.app_id, &self.user_id)
    }

    fn device_keys(&self) -> Result<DeviceKeys, Box<dyn std::error::Error>> {
        let record = self
            .device_keys
            .as_ref()
            .ok_or("device keys missing; re-run libresync init")?;
        let cert_der = BASE64
            .decode(record.cert_der.as_bytes())
            .map_err(|error| format!("failed to decode cert: {error}"))?;
        let key_der = BASE64
            .decode(record.key_der.as_bytes())
            .map_err(|error| format!("failed to decode key: {error}"))?;
        let keys = DeviceKeys::from_der(cert_der, key_der)?;
        Ok(keys)
    }

    fn is_paired(&self, device_id: &str) -> bool {
        self.devices.contains_key(device_id)
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
            entry.last_seen_unix_secs = Some(now_unix_secs());
        }
        if let Some(fingerprint) = fingerprint {
            entry.fingerprint = Some(fingerprint);
        }
    }
}

struct ConfigHandler {
    app_id: String,
    config_path: PathBuf,
    config: Arc<Mutex<Config>>,
    auto_accept: bool,
}

impl DeviceHandler for ConfigHandler {
    fn app_id(&self) -> &str {
        &self.app_id
    }

    fn is_paired(&self, identity: &Identity) -> bool {
        let config = self.config.lock().expect("config lock");
        config.is_paired(&identity.device_id)
    }

    fn approve_pair(&self, identity: &Identity) -> libresync::Result<bool> {
        self.approve_pair_with_fingerprint(identity, "")
    }

    fn device_keys(&self) -> libresync::Result<DeviceKeys> {
        let config = self.config.lock().expect("config lock");
        config
            .device_keys()
            .map_err(|error| libresync::Error::Protocol(error.to_string()))
    }

    fn is_paired_with_fingerprint(&self, identity: &Identity, fingerprint: &str) -> bool {
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

    fn approve_pair_with_fingerprint(
        &self,
        identity: &Identity,
        fingerprint: &str,
    ) -> libresync::Result<bool> {
        if identity.app_id != self.app_id {
            return Ok(false);
        }
        let accepted = if self.auto_accept {
            true
        } else {
            prompt_yes_no(&format!(
                "Pair with {} ({})? [y/N]: ",
                identity.device_id, identity.user_id
            ))
            .unwrap_or(false)
        };

        if accepted {
            let mut config = self.config.lock().expect("config lock");
            let fingerprint = if fingerprint.is_empty() {
                None
            } else {
                Some(fingerprint.to_string())
            };
            config.upsert_device(identity, None, fingerprint);
            save_config(&self.config_path, &config)
                .map_err(|error| libresync::Error::Protocol(error.to_string()))?;
        }

        Ok(accepted)
    }
}

struct StaticHandler {
    app_id: String,
    allowed: HashMap<String, String>,
    keys: DeviceKeys,
}

impl DeviceHandler for StaticHandler {
    fn app_id(&self) -> &str {
        &self.app_id
    }

    fn is_paired(&self, identity: &Identity) -> bool {
        self.allowed.contains_key(&identity.device_id)
    }

    fn approve_pair(&self, _identity: &Identity) -> libresync::Result<bool> {
        Ok(false)
    }

    fn device_keys(&self) -> libresync::Result<DeviceKeys> {
        Ok(self.keys.clone())
    }

    fn is_paired_with_fingerprint(&self, identity: &Identity, fingerprint: &str) -> bool {
        self.allowed
            .get(&identity.device_id)
            .map(|stored| stored == fingerprint)
            .unwrap_or(false)
    }
}

fn engine_for_config(config: &Config) -> Engine {
    let device_keys = config
        .device_keys()
        .expect("device keys should exist");
    let handler = Arc::new(StaticHandler {
        app_id: config.app_id.clone(),
        allowed: allowed_fingerprint_map(config),
        keys: device_keys,
    });
    Engine::new(
        EngineConfig::new(config.identity()),
        State::new(config.device_id.clone()),
        handler,
    )
}

fn allowed_fingerprint_map(config: &Config) -> HashMap<String, String> {
    config
        .devices
        .iter()
        .filter_map(|(device_id, record)| {
            record
                .fingerprint
                .as_ref()
                .map(|fingerprint| (device_id.clone(), fingerprint.clone()))
        })
        .collect()
}

fn main() {
    let cli = Cli::parse();
    let verbose = cli.verbose;
    if let Err(error) = run(cli) {
        report_error(&*error, verbose);
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Commands::Init {
            config,
            app_id,
            device_id,
            user_id,
            force,
        } => {
            let config = resolve_config_path(config);
            init_config(&config, &app_id, device_id, user_id, force)?;
        }
        Commands::Select { config, file } => {
            let config = resolve_config_path(config);
            select_file(&config, &file)?;
        }
        Commands::Discover {
            config,
            timeout_secs,
        } => {
            let config = resolve_config_path(config);
            discover_devices(&config, timeout_secs)?;
        }
        Commands::Pair {
            config,
            device,
            device_id,
            yes,
        } => {
            let config = resolve_config_path(config);
            pair_device(&config, device, device_id, yes)?;
        }
        Commands::Listen {
            config,
            listen,
            auto_accept,
            no_discovery,
            foreground,
            duration_secs,
        } => {
            let config = resolve_config_path(config);
            if foreground {
                listen_device(&config, listen, auto_accept, no_discovery, duration_secs)?;
            } else {
                spawn_background_listener(&config, listen, auto_accept, no_discovery, duration_secs)?;
            }
        }
        Commands::Refresh {
            config,
            device,
            device_id,
        } => {
            let config = resolve_config_path(config);
            refresh_file(&config, device, device_id)?;
        }
        Commands::Watch {
            config,
            listen,
            interval_secs,
            debounce_ms,
            no_discover,
            no_listen,
        } => {
            let config = resolve_config_path(config);
            watch_file(&config, listen, interval_secs, debounce_ms, no_discover, no_listen)?;
        }
        Commands::Stop { config } => {
            let config = resolve_config_path(config);
            stop_listener(&config)?;
        }
        Commands::Status {
            config,
            no_discover,
            timeout_secs,
        } => {
            let config = resolve_config_path(config);
            status(&config, !no_discover, timeout_secs)?;
        }
    }

    Ok(())
}

fn report_error(error: &dyn StdError, verbose: bool) {
    eprintln!("Error: {error}");
    if verbose {
        eprintln!("Debug: {error:?}");
        let mut index = 1;
        let mut source = error.source();
        while let Some(cause) = source {
            eprintln!("Caused by ({index}): {cause}");
            eprintln!("Cause debug ({index}): {cause:?}");
            source = cause.source();
            index += 1;
        }
    } else {
        eprintln!("Hint: re-run with --verbose for debug details.");
    }
}

fn init_config(
    path: &Path,
    app_id: &str,
    device_id: Option<String>,
    user_id: Option<String>,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if path.exists() && !force {
        return Err(format!("config file already exists: {}", path.display()).into());
    }

    let device_id = device_id.unwrap_or_else(generate_device_id);
    let user_id = user_id.unwrap_or_else(generate_user_id);
    let state_path = default_state_path(path);
    let identity = Identity::new(&device_id, app_id, &user_id);
    let device_keys = DeviceKeys::generate(&identity)
        .map_err(|error| io::Error::new(io::ErrorKind::Other, error.to_string()))?;

    let config = Config {
        device_id,
        app_id: app_id.to_string(),
        user_id,
        state_path,
        data_path: None,
        device_keys: Some(DeviceKeysRecord::from_keys(&device_keys)),
        devices: BTreeMap::new(),
    };

    save_config(path, &config)?;

    let state = State::new(config.device_id.clone());
    state.save(&config.state_path)?;

    println!(
        "Initialized config at {} (device: {}, user: {})",
        path.display(),
        config.device_id,
        config.user_id
    );

    Ok(())
}

fn select_file(path: &Path, file: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_config(path)?;
    config.data_path = Some(file.to_path_buf());
    save_config(path, &config)?;
    ensure_json_file(file)?;
    println!("Selected file {}", file.display());
    Ok(())
}

fn discover_devices(path: &Path, timeout_secs: u64) -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config(path)?;
    let engine = engine_for_config(&config);
    let devices = engine.discover_devices_with_timeout(Duration::from_secs(timeout_secs))?;

    if devices.is_empty() {
        println!("No devices found for app {}", config.app_id);
        return Ok(());
    }

    for device in devices {
        println!(
            "{} ({}) at {}",
            device.identity.device_id,
            device.identity.user_id,
            device
                .address
                .map(|addr| addr.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        );
    }

    Ok(())
}

fn pair_device(
    path: &Path,
    device: Option<SocketAddr>,
    device_id: Option<String>,
    auto_accept: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_config(path)?;
    let device = resolve_device_address(&config, device, device_id.as_deref())?;
    let engine = engine_for_config(&config);
    let remote_device = engine.request_pair(device)?;
    let remote_identity = remote_device.identity;

    let local_accept = if auto_accept {
        true
    } else {
        prompt_yes_no(&format!(
            "Confirm pairing with {} ({})? [y/N]: ",
            remote_identity.device_id, remote_identity.user_id
        ))
        .unwrap_or(false)
    };

    if !local_accept {
        return Err("pairing aborted locally".into());
    }

    config.upsert_device(
        &remote_identity,
        Some(device),
        remote_device.fingerprint.clone(),
    );
    save_config(path, &config)?;

    println!(
        "Paired with {} ({})",
        remote_identity.device_id, remote_identity.user_id
    );

    Ok(())
}

fn listen_device(
    path: &Path,
    listen: SocketAddr,
    auto_accept: bool,
    no_discovery: bool,
    duration_secs: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config(path)?;
    let identity = config.identity();
    let config = Arc::new(Mutex::new(config));

    let state = load_or_init_state(&config.lock().expect("config lock"))?;

    let handler = Arc::new(ConfigHandler {
        app_id: identity.app_id.clone(),
        config_path: path.to_path_buf(),
        config: Arc::clone(&config),
        auto_accept,
    });

    let mut engine = Engine::new(
        EngineConfig::new(identity.clone()).with_listen_addr(listen),
        state,
        handler,
    );

    let adapter = {
        let cfg = config.lock().expect("config lock");
        cfg.data_path
            .clone()
            .map(|path| Arc::new(JsonFileAdapter::new(FILE_KEY, path)))
    };

    if let Some(adapter) = &adapter {
        let adapter_trait: Arc<dyn DataAdapter> = adapter.clone();
        engine.register_adapter(adapter_trait)?;
    }

    let listener_addr = engine.start_listening()?;
    let state = engine.state();

    let mdns = if no_discovery {
        None
    } else {
        Some(libresync::register_mdns(&identity, listener_addr)?)
    };

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    ctrlc::set_handler(move || {
        running_clone.store(false, Ordering::SeqCst);
    })?;

    let adapter_watch = if adapter.is_some() {
        let state_path = config.lock().expect("config lock").state_path.clone();
        Some(engine.watch(FILE_KEY, state_path, Duration::from_millis(250))?)
    } else {
        None
    };

    println!(
        "Listening on {} (device: {}, user: {})",
        listener_addr,
        identity.device_id,
        identity.user_id
    );

    let deadline = duration_secs.map(|secs| Instant::now() + Duration::from_secs(secs));

    while running.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(500));
        if let Some(deadline) = deadline {
            if Instant::now() >= deadline {
                break;
            }
        }
    }

    let listener = engine.stop_listening();
    running.store(false, Ordering::SeqCst);
    if let Some(watch) = adapter_watch {
        let _ = watch.stop();
    }
    let state = state.lock().expect("state poisoned");
    state.save(&config.lock().expect("config lock").state_path)?;
    drop(mdns);
    listener?;

    Ok(())
}

fn spawn_background_listener(
    path: &Path,
    listen: SocketAddr,
    auto_accept: bool,
    no_discovery: bool,
    duration_secs: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    let _config = load_config(path)?;

    let log_path = log_path_for_config(path);
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let log_err = log.try_clone()?;

    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("listen")
        .arg("--foreground")
        .arg("--config")
        .arg(path)
        .arg("--listen")
        .arg(listen.to_string())
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err);

    if auto_accept {
        cmd.arg("--auto-accept");
    }

    if no_discovery {
        cmd.arg("--no-discovery");
    }

    if let Some(secs) = duration_secs {
        cmd.arg("--duration-secs").arg(secs.to_string());
    }

    let child = cmd.spawn()?;
    let pid = child.id();

    let pid_path = pid_path_for_config(path);
    fs::write(&pid_path, pid.to_string())?;

    println!("Listener started in the background.");
    println!("PID: {pid} (saved to {})", pid_path.display());
    println!("Logs: {}", log_path.display());
    println!("Stop: kill {pid}");

    Ok(())
}

fn stop_listener(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let _config = load_config(path)?;

    let pid_path = pid_path_for_config(path);
    let pid_raw = fs::read_to_string(&pid_path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "failed to read listener PID file {}: {error}",
                pid_path.display()
            ),
        )
    })?;
    let pid_value = pid_raw
        .trim()
        .parse::<u32>()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;

    let mut system = System::new();
    system.refresh_processes();
    let pid = Pid::from_u32(pid_value);

    if let Some(process) = system.process(pid) {
        let name = process.name().to_lowercase();
        if !name.contains("libresync") {
            return Err(format!(
                "pid {pid_value} is not a libresync process ({})",
                process.name()
            )
            .into());
        }
        if !process.kill() {
            return Err(format!("failed to terminate process {pid_value}").into());
        }
        fs::remove_file(&pid_path).ok();
        println!("Stopped listener process {pid_value}.");
        return Ok(());
    }

    fs::remove_file(&pid_path).ok();
    println!("No listener process found; removed stale PID file.");
    Ok(())
}

fn refresh_file(
    path: &Path,
    device: Option<SocketAddr>,
    device_id: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_config(path)?;
    let device = resolve_device_address(&config, device, device_id.as_deref())?;
    let data_path = config
        .data_path
        .clone()
        .ok_or("no file selected; run libresync select")?;

    let device_keys = config.device_keys()?;
    let state = load_or_init_state(&config)?;
    let handler = Arc::new(StaticHandler {
        app_id: config.app_id.clone(),
        allowed: allowed_fingerprint_map(&config),
        keys: device_keys,
    });
    let mut engine = Engine::new(EngineConfig::new(config.identity()), state, handler);
    let adapter = Arc::new(JsonFileAdapter::new(FILE_KEY, data_path));
    let adapter_trait: Arc<dyn DataAdapter> = adapter;
    engine.register_adapter(adapter_trait)?;

    let remote_device = engine.sync_now(device, FILE_KEY)?;

    let state = engine.state();
    state
        .lock()
        .expect("state lock")
        .save(&config.state_path)?;
    config.upsert_device(
        &remote_device.identity,
        Some(device),
        remote_device.fingerprint.clone(),
    );
    save_config(path, &config)?;

    println!(
        "Refreshed with {} ({})",
        remote_device.identity.device_id, remote_device.identity.user_id
    );

    Ok(())
}

fn status(
    path: &Path,
    discover: bool,
    timeout_secs: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_config(path)?;

    println!("Device ID: {}", config.device_id);
    println!("User ID:   {}", config.user_id);
    println!("App ID:    {}", config.app_id);
    match &config.data_path {
        Some(path) => println!("Selected file: {}", path.display()),
        None => println!("Selected file: (none)"),
    }
    print_listener_status(path)?;

    let engine = engine_for_config(&config);
    let mut discovered: Vec<DeviceInfo> = Vec::new();
    if discover {
        discovered = engine.discover_devices_with_timeout(Duration::from_secs(timeout_secs))?;
    }

    let discovered_map: HashMap<String, DeviceInfo> = discovered
        .iter()
        .cloned()
        .map(|device| (device.identity.device_id.clone(), device))
        .collect();

    if discover {
        println!("\nDiscovered now:");
        if discovered_map.is_empty() {
            println!("  (none)");
        } else {
            for device in discovered_map.values() {
                let paired = config.devices.contains_key(&device.identity.device_id);
                let status = if paired { "paired" } else { "unpaired" };
                let address = device
                    .address
                    .map(|addr| addr.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                println!(
                    "  {} ({}) at {} [{}]",
                    device.identity.device_id,
                    device.identity.user_id,
                    address,
                    status
                );
            }
        }
    }

    if !config.devices.is_empty() {
        println!("\nPaired devices:");
    } else {
        println!("\nPaired devices: (none)");
    }

    for record in config.devices.values_mut() {
        if let Some(found) = discovered_map.get(&record.device_id) {
            if let Some(address) = found.address {
                record.last_seen_addr = Some(address.to_string());
                record.last_seen_unix_secs = Some(now_unix_secs());
            }
        }
        let addr = record
            .last_seen_addr
            .as_deref()
            .unwrap_or("unknown");
        let connected = if discovered_map.contains_key(&record.device_id) {
            "connected"
        } else {
            "not connected"
        };
        let last_seen = record
            .last_seen_unix_secs
            .map(format_last_seen)
            .unwrap_or_else(|| "last seen: unknown".to_string());
        println!(
            "  {} ({}) at {} [{}] {}",
            record.device_id, record.user_id, addr, connected, last_seen
        );
    }

    if discover && !discovered_map.is_empty() {
        save_config(path, &config)?;
    }

    Ok(())
}

fn print_listener_status(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let pid_path = pid_path_for_config(path);
    let log_path = log_path_for_config(path);

    let pid_raw = match fs::read_to_string(&pid_path) {
        Ok(pid) => pid,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            println!("Listener: stopped (no PID file)");
            println!("Listener log: {}", log_path.display());
            return Ok(());
        }
        Err(error) => {
            println!(
                "Listener: unknown (failed to read PID file: {})",
                error
            );
            println!("Listener log: {}", log_path.display());
            return Ok(());
        }
    };

    let pid_value = match pid_raw.trim().parse::<u32>() {
        Ok(pid) => pid,
        Err(_) => {
            println!("Listener: invalid PID file (value: {})", pid_raw.trim());
            println!("Listener log: {}", log_path.display());
            return Ok(());
        }
    };

    let mut system = System::new();
    system.refresh_processes();
    let pid = Pid::from_u32(pid_value);

    if let Some(process) = system.process(pid) {
        let name = process.name().to_lowercase();
        if name.contains("libresync") {
            println!("Listener: running (pid {pid_value})");
            println!("Listener log: {}", log_path.display());
            return Ok(());
        }
        println!(
            "Listener: stale PID (pid {pid_value} belongs to {})",
            process.name()
        );
        println!("Listener log: {}", log_path.display());
        return Ok(());
    }

    println!("Listener: stale PID (pid {pid_value} not running)");
    println!("Listener log: {}", log_path.display());
    Ok(())
}

fn load_config(path: &Path) -> Result<Config, Box<dyn std::error::Error>> {
    let data = fs::read(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to read config {}: {error}", path.display()),
        )
    })?;
    let mut config: Config = serde_json::from_slice(&data).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to parse config {}: {error}", path.display()),
        )
    })?;

    if config.device_keys.is_none() {
        let identity = config.identity();
        let keys = DeviceKeys::generate(&identity)
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error.to_string()))?;
        config.device_keys = Some(DeviceKeysRecord::from_keys(&keys));
        save_config(path, &config)?;
    }

    Ok(config)
}

fn save_config(path: &Path, config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let serialized = serde_json::to_vec_pretty(config).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to serialize config {}: {error}", path.display()),
        )
    })?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "failed to create config directory {}: {error}",
                    parent.display()
                ),
            )
        })?;
    }
    fs::write(path, serialized).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to write config {}: {error}", path.display()),
        )
    })?;
    Ok(())
}

fn default_state_path(config_path: &Path) -> PathBuf {
    config_path.with_extension("state.json")
}

fn load_or_init_state(config: &Config) -> Result<State, Box<dyn std::error::Error>> {
    if config.state_path.exists() {
        let state = State::load(&config.state_path)?;
        if state.device_id != config.device_id {
            return Err("state device id does not match config".into());
        }
        Ok(state)
    } else {
        let state = State::new(config.device_id.clone());
        state.save(&config.state_path)?;
        Ok(state)
    }
}

fn ensure_json_file(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("failed to create data directory {}: {error}", parent.display()),
            )
        })?;
    }
    fs::write(path, b"{}").map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to write data file {}: {error}", path.display()),
        )
    })?;
    Ok(())
}

fn prompt_yes_no(prompt: &str) -> Result<bool, io::Error> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let response = input.trim().to_lowercase();
    Ok(matches!(response.as_str(), "y" | "yes"))
}

fn resolve_device_address(
    config: &Config,
    device: Option<SocketAddr>,
    device_id: Option<&str>,
) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    resolve_device_address_with_timeout(config, device, device_id, Duration::from_secs(3))
}

fn resolve_device_address_with_timeout(
    config: &Config,
    device: Option<SocketAddr>,
    device_id: Option<&str>,
    timeout: Duration,
) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    if let Some(device) = device {
        return Ok(device);
    }

    let engine = engine_for_config(config);
    let devices = engine.discover_devices_with_timeout(timeout)?;
    if devices.is_empty() {
        return Err("no devices found on the LAN".into());
    }

    if let Some(device_id) = device_id {
        let matched = devices
            .into_iter()
            .find(|device| device.identity.device_id == device_id)
            .ok_or_else(|| format!("device not found: {device_id}"))?;
        return matched
            .address
            .ok_or_else(|| "device address unavailable".into());
    }

    if devices.len() == 1 {
        return devices[0]
            .address
            .ok_or_else(|| "device address unavailable".into());
    }

    let selection = prompt_select_device(&devices)?;
    selection
        .address
        .ok_or_else(|| "device address unavailable".into())
}

fn prompt_select_device(devices: &[DeviceInfo]) -> Result<DeviceInfo, io::Error> {
    println!("Select a device:");
    for (index, device) in devices.iter().enumerate() {
        let address = device
            .address
            .map(|addr| addr.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        println!(
            "  {}) {} ({}) at {}",
            index + 1,
            device.identity.device_id,
            device.identity.user_id,
            address
        );
    }

    loop {
        print!("Enter selection [1-{}]: ", devices.len());
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let selection = input.trim().parse::<usize>().ok();
        if let Some(choice) = selection {
            if choice >= 1 && choice <= devices.len() {
                return Ok(devices[choice - 1].clone());
            }
        }
        println!("Invalid selection. Try again.");
    }
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn format_last_seen(ts: u64) -> String {
    let now = now_unix_secs() as i64;
    let ts_i64 = ts as i64;
    let delta = now.saturating_sub(ts_i64);
    let relative = format_relative(delta);

    match Local.timestamp_opt(ts_i64, 0).single() {
        Some(local) => format!(
            "last seen: {} ({})",
            local.format("%Y-%m-%d %H:%M:%S"),
            relative
        ),
        None => format!("last seen: {ts} ({relative})"),
    }
}

fn format_relative(delta_secs: i64) -> String {
    if delta_secs <= 0 {
        let future = delta_secs.unsigned_abs();
        return format_relative_inner(future, true);
    }
    format_relative_inner(delta_secs as u64, false)
}

fn format_relative_inner(secs: u64, future: bool) -> String {
    let (value, unit) = if secs < 10 {
        return "just now".to_string();
    } else if secs < 60 {
        (secs, "s")
    } else if secs < 60 * 60 {
        (secs / 60, "m")
    } else if secs < 60 * 60 * 24 {
        (secs / (60 * 60), "h")
    } else if secs < 60 * 60 * 24 * 7 {
        (secs / (60 * 60 * 24), "d")
    } else {
        (secs / (60 * 60 * 24 * 7), "w")
    };

    if future {
        format!("in {value}{unit}")
    } else {
        format!("{value}{unit} ago")
    }
}

fn watch_file(
    path: &Path,
    listen: SocketAddr,
    interval_secs: u64,
    debounce_ms: u64,
    no_discover: bool,
    no_listen: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_config(path)?;
    let data_path = config
        .data_path
        .clone()
        .ok_or("no file selected; run libresync select")?;
    ensure_json_file(&data_path)?;

    println!("Watching {}", data_path.display());
    println!(
        "Paired devices: {}",
        if config.devices.is_empty() {
            "none".to_string()
        } else {
            config.devices.len().to_string()
        }
    );
    println!(
        "Refresh interval: {}s (debounce: {}ms)",
        interval_secs, debounce_ms
    );
    if !no_listen {
        ensure_listener_running(path, listen)?;
    }

    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = RecommendedWatcher::new(tx, notify::Config::default())?;
    watcher.watch(&data_path, RecursiveMode::NonRecursive)?;

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();
    ctrlc::set_handler(move || {
        running_clone.store(false, Ordering::SeqCst);
    })?;

    let mut last_event = None::<Instant>;
    let mut last_refresh = Instant::now()
        .checked_sub(Duration::from_secs(interval_secs))
        .unwrap_or_else(Instant::now);

    while running.load(Ordering::SeqCst) {
        let timeout = Duration::from_millis(200);
        let event = rx.recv_timeout(timeout);
        let mut local_changed = false;

        if let Ok(_event) = event {
            let now = Instant::now();
            let debounce = Duration::from_millis(debounce_ms);
            if last_event
                .map(|last| now.duration_since(last) >= debounce)
                .unwrap_or(true)
            {
                local_changed = true;
                last_event = Some(now);
            }
        }

        let interval_due = last_refresh.elapsed() >= Duration::from_secs(interval_secs);
        if local_changed || interval_due {
            if local_changed {
                let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
                println!("Change detected at {timestamp}; refreshing paired devices.");
            }
            let refresh_result =
                refresh_all_devices(path, &mut config, !no_discover);
            match refresh_result {
                Ok(changed) => {
                    if changed {
                        let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
                        println!("Remote change detected at {timestamp}; file updated.");
                    }
                }
                Err(error) => {
                    eprintln!("Refresh error: {error}");
                }
            }
            last_refresh = Instant::now();
        }
    }

    Ok(())
}

fn ensure_listener_running(
    path: &Path,
    listen: SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    if is_port_listening(listen)? {
        println!("Listener: already running on {}", listen);
        return Ok(());
    }

    match spawn_background_listener(path, listen, false, false, None) {
        Ok(()) => Ok(()),
        Err(error) => {
            if let Some(io_error) = error.downcast_ref::<io::Error>() {
                if io_error.kind() == io::ErrorKind::AddrInUse {
                    println!("Listener: already running on {}", listen);
                    return Ok(());
                }
            }
            Err(error)
        }
    }
}

fn is_port_listening(addr: SocketAddr) -> Result<bool, Box<dyn std::error::Error>> {
    let mut addr = addr;
    if addr.ip().is_unspecified() {
        addr.set_ip(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    }
    match std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(300)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::TimedOut => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn refresh_all_devices(
    path: &Path,
    config: &mut Config,
    discover: bool,
) -> Result<bool, Box<dyn std::error::Error>> {
    if config.devices.is_empty() {
        return Ok(false);
    }

    let discovered = if discover {
        let engine = engine_for_config(config);
        engine.discover_devices_with_timeout(Duration::from_secs(2))?
    } else {
        Vec::new()
    };
    let discovered_map: HashMap<String, SocketAddr> = discovered
        .into_iter()
        .filter_map(|device| device.address.map(|addr| (device.identity.device_id, addr)))
        .collect();

    let mut any_changed = false;
    let device_ids: Vec<String> = config.devices.keys().cloned().collect();
    for device_id in device_ids {
        let addresses = resolve_addresses_for_watch(config, &device_id, &discovered_map);
        if addresses.is_empty() {
            continue;
        }

        let mut last_error = None::<String>;
        let mut refreshed = false;
        for addr in addresses {
            match refresh_with_address(path, config, addr) {
                Ok(changed) => {
                    if changed {
                        any_changed = true;
                    }
                    refreshed = true;
                    break;
                }
                Err(error) => {
                    last_error = Some(error.to_string());
                }
            }
        }

        if !refreshed {
            if let Some(error) = last_error {
                if error.contains("Connection refused") {
                    eprintln!(
                        "Refresh error for {device_id}: {error} (is the listener running?)"
                    );
                } else {
                    eprintln!("Refresh error for {device_id}: {error}");
                }
            }
        }
    }

    Ok(any_changed)
}

fn resolve_addresses_for_watch(
    config: &Config,
    device_id: &str,
    discovered_map: &HashMap<String, SocketAddr>,
) -> Vec<SocketAddr> {
    let mut addresses = Vec::new();
    if let Some(discovered) = discovered_map.get(device_id) {
        addresses.push(*discovered);
    }
    if let Some(record) = config.devices.get(device_id) {
        if let Some(addr) = record.last_seen_addr.as_deref() {
            if let Ok(parsed) = addr.parse::<SocketAddr>() {
                if !addresses.contains(&parsed) {
                    addresses.push(parsed);
                }
            }
        }
    }
    addresses
}

fn refresh_with_address(
    path: &Path,
    config: &mut Config,
    device: SocketAddr,
) -> Result<bool, Box<dyn std::error::Error>> {
    let data_path = config
        .data_path
        .clone()
        .ok_or("no file selected; run libresync select")?;

    let before_bytes = fs::read(&data_path).unwrap_or_default();
    let device_keys = config.device_keys()?;
    let state = load_or_init_state(config)?;
    let handler = Arc::new(StaticHandler {
        app_id: config.app_id.clone(),
        allowed: allowed_fingerprint_map(config),
        keys: device_keys,
    });
    let mut engine = Engine::new(EngineConfig::new(config.identity()), state, handler);
    let adapter = Arc::new(JsonFileAdapter::new(FILE_KEY, data_path.clone()));
    let adapter_trait: Arc<dyn DataAdapter> = adapter;
    engine.register_adapter(adapter_trait)?;

    let remote_device = engine.sync_now(device, FILE_KEY)?;
    let after_bytes = fs::read(&data_path).unwrap_or_default();
    let changed = before_bytes != after_bytes;

    let state = engine.state();
    state
        .lock()
        .expect("state lock")
        .save(&config.state_path)?;
    config.upsert_device(
        &remote_device.identity,
        Some(device),
        remote_device.fingerprint.clone(),
    );
    save_config(path, config)?;
    Ok(changed)
}

fn generate_device_id() -> String {
    let mut rng = rand::thread_rng();
    let words = DEVICE_WORDS.choose_multiple(&mut rng, 3).cloned().collect::<Vec<_>>();
    words.join("-")
}

fn generate_user_id() -> String {
    let mut rng = rand::thread_rng();
    let adjective = ADJECTIVES.choose(&mut rng).unwrap_or(&"calm");
    let noun = NOUNS.choose(&mut rng).unwrap_or(&"forest");
    format!("{}-{}", adjective, noun)
}

const DEVICE_WORDS: &[&str] = &[
    "amber", "anchor", "atlas", "aurora", "blossom", "breeze", "canyon", "cedar",
    "cliff", "comet", "coral", "cove", "dawn", "delta", "ember", "fable", "field",
    "fjord", "forest", "glade", "harbor", "haven", "island", "keystone", "lagoon",
    "lumen", "meadow", "mesa", "mist", "nova", "orbit", "pine", "prairie", "ridge",
    "river", "sage", "sierra", "signal", "sky", "solace", "spark", "stone", "summit",
    "tide", "vale", "valley", "vista", "wild", "zephyr",
];

const ADJECTIVES: &[&str] = &[
    "brisk", "calm", "clear", "cozy", "gentle", "glad", "golden", "grand", "kind",
    "lively", "mellow", "neat", "nimble", "proud", "quiet", "steady", "swift",
    "tender", "true", "vivid", "warm",
];

const NOUNS: &[&str] = &[
    "brook", "cascade", "canyon", "cloud", "crest", "dune", "field", "forest",
    "garden", "grove", "harbor", "island", "meadow", "orchard", "path", "peak",
    "prairie", "ridge", "river", "signal", "summit", "trail", "vale",
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::net::SocketAddrV4;

    #[test]
    fn device_id_has_three_parts() {
        let id = generate_device_id();
        assert_eq!(id.split('-').count(), 3);
    }

    #[test]
    fn user_id_has_two_parts() {
        let id = generate_user_id();
        assert_eq!(id.split('-').count(), 2);
    }

    #[test]
    fn resolve_device_address_prefers_explicit_device() {
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 52345));
        let identity = Identity::new("local-device", APP_ID_DEFAULT, "user");
        let device_keys = DeviceKeysRecord::from_keys(
            &DeviceKeys::generate(&identity).expect("device keys"),
        );
        let config = Config {
            device_id: "local-device".to_string(),
            app_id: APP_ID_DEFAULT.to_string(),
            user_id: "user".to_string(),
            state_path: PathBuf::from("state.json"),
            data_path: None,
            device_keys: Some(device_keys),
            devices: BTreeMap::new(),
        };
        let resolved = resolve_device_address_with_timeout(
            &config,
            Some(addr),
            None,
            Duration::from_millis(1),
        )
        .expect("resolve");
        assert_eq!(resolved, addr);
    }

    #[test]
    fn resolve_device_address_errors_when_none_found() {
        let identity = Identity::new("local-device", APP_ID_DEFAULT, "user");
        let device_keys = DeviceKeysRecord::from_keys(
            &DeviceKeys::generate(&identity).expect("device keys"),
        );
        let config = Config {
            device_id: "local-device".to_string(),
            app_id: APP_ID_DEFAULT.to_string(),
            user_id: "user".to_string(),
            state_path: PathBuf::from("state.json"),
            data_path: None,
            device_keys: Some(device_keys),
            devices: BTreeMap::new(),
        };
        let error = resolve_device_address_with_timeout(
            &config,
            None,
            Some("missing-device"),
            Duration::from_millis(5),
        )
        .expect_err("expected error");
        let message = error.to_string();
        assert!(
            message.contains("no devices found") || message.contains("device not found"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn upsert_device_sets_last_seen_fields() {
        let identity = Identity::new("local", APP_ID_DEFAULT, "user");
        let device_keys = DeviceKeysRecord::from_keys(
            &DeviceKeys::generate(&identity).expect("device keys"),
        );
        let mut config = Config {
            device_id: "local".to_string(),
            app_id: APP_ID_DEFAULT.to_string(),
            user_id: "user".to_string(),
            state_path: PathBuf::from("state.json"),
            data_path: None,
            device_keys: Some(device_keys),
            devices: BTreeMap::new(),
        };

        let identity = Identity::new("remote", APP_ID_DEFAULT, "remote-user");
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 52345));
        config.upsert_device(&identity, Some(addr), None);

        let record = config.devices.get("remote").expect("record");
        assert_eq!(record.last_seen_addr.as_deref(), Some("127.0.0.1:52345"));
        assert!(record.last_seen_unix_secs.unwrap_or(0) > 0);
    }

}
