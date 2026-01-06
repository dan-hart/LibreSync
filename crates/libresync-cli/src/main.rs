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
    AppKey, BackupManager, DataAdapter, DataAdapterBackup, DeviceHandler, DeviceInfo, DeviceKeys,
    Engine, EngineConfig, FileSnapshotStore, Identity, JsonFileAdapter, RestoreOptions, State,
    summarize_snapshot_diff,
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
        about = "Revoke pairing with a device.",
        long_about = "Removes a paired device from the local allowlist. The device will need to pair again before any future refresh."
    )]
    Unpair {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory).",
            long_help = "Config file that includes the paired devices allowlist. Defaults to the OS config directory when omitted."
        )]
        config: Option<PathBuf>,
        #[arg(
            long,
            help = "Device ID to revoke.",
            long_help = "Device ID to remove from the allowlist."
        )]
        device_id: String,
        #[arg(long, short, help = "Skip the confirmation prompt.")]
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
        about = "Manage encrypted backups and restore points for the selected data.",
        long_about = "Create, list, and restore encrypted snapshots. Backups are opt-in per app, and restores require an explicit config flag plus --confirm."
    )]
    Backup {
        #[command(subcommand)]
        command: BackupCommands,
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

#[derive(Subcommand)]
enum BackupCommands {
    #[command(
        about = "Configure backup behavior for this app.",
        long_about = "Enable backups and optionally allow restores. This is a required opt-in per app."
    )]
    Configure {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory)."
        )]
        config: Option<PathBuf>,
        #[arg(
            long,
            help = "Enable encrypted backups for this app."
        )]
        enable: bool,
        #[arg(
            long,
            help = "Allow restores for this app (requires explicit confirmation at restore time)."
        )]
        allow_restore: bool,
        #[arg(
            long,
            help = "Optional backup directory override."
        )]
        dir: Option<PathBuf>,
    },
    #[command(
        about = "Create an encrypted snapshot.",
        long_about = "Create a new encrypted snapshot of the selected data for backups."
    )]
    Snapshot {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory)."
        )]
        config: Option<PathBuf>,
        #[arg(
            long,
            default_value = FILE_KEY,
            help = "Adapter ID to snapshot (default: file)."
        )]
        adapter_id: String,
        #[arg(
            long,
            help = "Optional note to attach to the snapshot."
        )]
        note: Option<String>,
    },
    #[command(
        about = "List encrypted snapshots.",
        long_about = "List encrypted snapshots stored for the selected adapter."
    )]
    List {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory)."
        )]
        config: Option<PathBuf>,
        #[arg(
            long,
            default_value = FILE_KEY,
            help = "Adapter ID to list (default: file)."
        )]
        adapter_id: String,
    },
    #[command(
        about = "Preview the diff between a snapshot and current state.",
        long_about = "Shows a diff summary between a snapshot and the current state without restoring."
    )]
    Preview {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory)."
        )]
        config: Option<PathBuf>,
        #[arg(
            long,
            default_value = FILE_KEY,
            help = "Adapter ID to preview (default: file)."
        )]
        adapter_id: String,
        #[arg(
            long,
            help = "Snapshot ID to preview."
        )]
        snapshot_id: String,
    },
    #[command(
        about = "Restore an encrypted snapshot.",
        long_about = "Restore a snapshot into the selected data. Requires backup allow-restore config, --confirm, and --confirm-id."
    )]
    Restore {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory)."
        )]
        config: Option<PathBuf>,
        #[arg(
            long,
            default_value = FILE_KEY,
            help = "Adapter ID to restore (default: file)."
        )]
        adapter_id: String,
        #[arg(
            long,
            help = "Snapshot ID to restore."
        )]
        snapshot_id: String,
        #[arg(
            long,
            help = "Confirm the restore action."
        )]
        confirm: bool,
        #[arg(
            long,
            help = "Confirm snapshot ID (must match the snapshot_id)."
        )]
        confirm_id: Option<String>,
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
    app_key: Option<String>,
    #[serde(default)]
    backup_enabled: bool,
    #[serde(default)]
    backup_allow_restore: bool,
    #[serde(default)]
    backup_dir: Option<PathBuf>,
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

    fn app_key(&self) -> Result<AppKey, Box<dyn std::error::Error>> {
        let encoded = self
            .app_key
            .as_ref()
            .ok_or("app key missing; re-run libresync init")?;
        let bytes = BASE64
            .decode(encoded.as_bytes())
            .map_err(|error| format!("failed to decode app key: {error}"))?;
        let key = AppKey::from_slice(&bytes)?;
        Ok(key)
    }

    fn set_app_key(&mut self, key: &AppKey) {
        self.app_key = Some(BASE64.encode(key.as_bytes()));
    }

    fn backup_dir(&self, config_path: &Path) -> PathBuf {
        self.backup_dir
            .clone()
            .unwrap_or_else(|| default_backup_dir(config_path))
    }

    fn set_backup_dir(&mut self, path: PathBuf) {
        self.backup_dir = Some(path);
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

    fn app_key(&self) -> libresync::Result<AppKey> {
        let config = self.config.lock().expect("config lock");
        config
            .app_key()
            .map_err(|error| libresync::Error::Protocol(error.to_string()))
    }

    fn set_app_key(&self, app_key: &AppKey) -> libresync::Result<()> {
        let mut config = self.config.lock().expect("config lock");
        config.set_app_key(app_key);
        save_config(&self.config_path, &config)
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
    app_key: AppKey,
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

    fn app_key(&self) -> libresync::Result<AppKey> {
        Ok(self.app_key.clone())
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
    let app_key = config.app_key().expect("app key should exist");
    let handler = Arc::new(StaticHandler {
        app_id: config.app_id.clone(),
        allowed: allowed_fingerprint_map(config),
        keys: device_keys,
        app_key,
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
        Commands::Unpair {
            config,
            device_id,
            yes,
        } => {
            let config = resolve_config_path(config);
            unpair_device(&config, &device_id, yes)?;
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
        Commands::Backup { command } => match command {
            BackupCommands::Configure {
                config,
                enable,
                allow_restore,
                dir,
            } => {
                let config = resolve_config_path(config);
                configure_backups(&config, enable, allow_restore, dir)?;
            }
            BackupCommands::Snapshot {
                config,
                adapter_id,
                note,
            } => {
                let config = resolve_config_path(config);
                create_backup_snapshot(&config, &adapter_id, note)?;
            }
            BackupCommands::List { config, adapter_id } => {
                let config = resolve_config_path(config);
                list_backup_snapshots(&config, &adapter_id)?;
            }
            BackupCommands::Preview {
                config,
                adapter_id,
                snapshot_id,
            } => {
                let config = resolve_config_path(config);
                preview_backup_snapshot(&config, &adapter_id, &snapshot_id)?;
            }
            BackupCommands::Restore {
                config,
                adapter_id,
                snapshot_id,
                confirm,
                confirm_id,
            } => {
                let config = resolve_config_path(config);
                restore_backup_snapshot(&config, &adapter_id, &snapshot_id, confirm, confirm_id)?;
            }
        },
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
    let app_key = AppKey::generate()
        .map_err(|error| io::Error::new(io::ErrorKind::Other, error.to_string()))?;

    let config = Config {
        device_id,
        app_id: app_id.to_string(),
        user_id,
        state_path,
        data_path: None,
        device_keys: Some(DeviceKeysRecord::from_keys(&device_keys)),
        app_key: Some(BASE64.encode(app_key.as_bytes())),
        backup_enabled: false,
        backup_allow_restore: false,
        backup_dir: None,
        devices: BTreeMap::new(),
    };

    save_config(path, &config)?;

    let state = State::new(config.device_id.clone());
    state.save_encrypted(&app_key, &config.state_path)?;

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

fn configure_backups(
    path: &Path,
    enable: bool,
    allow_restore: bool,
    dir: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_config(path)?;
    let mut changed = false;

    if enable {
        config.backup_enabled = true;
        changed = true;
    }
    if allow_restore {
        config.backup_allow_restore = true;
        changed = true;
    }
    if let Some(dir) = dir {
        config.set_backup_dir(dir);
        changed = true;
    }

    if !changed {
        return Err("no backup changes requested; use --enable and/or --allow-restore".into());
    }

    if config.backup_enabled && config.backup_dir.is_none() {
        config.set_backup_dir(default_backup_dir(path));
    }

    save_config(path, &config)?;

    println!("Backups enabled: {}", config.backup_enabled);
    println!("Restore allowed: {}", config.backup_allow_restore);
    println!("Backup dir: {}", config.backup_dir(path).display());

    Ok(())
}

fn create_backup_snapshot(
    path: &Path,
    adapter_id: &str,
    note: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_config(path)?;
    let backup_dir = ensure_backup_dir(path, &mut config)?;
    if !config.backup_enabled {
        return Err("backups are not enabled; run libresync backup configure --enable".into());
    }

    let app_key = config.app_key()?;
    let store = FileSnapshotStore::new(&backup_dir)?;
    let manager = BackupManager::new(app_key, Arc::new(store));

    let (adapter, backup_adapter) = backup_adapter_for_config(&config, adapter_id)?;
    let mut state = load_or_init_state(&config)?;
    adapter.load_into_state(&mut state)?;

    let metadata = manager.create_snapshot(&backup_adapter, &state, note)?;
    state.save_encrypted(&config.app_key()?, &config.state_path)?;

    println!(
        "Created snapshot {} (entries: {})",
        metadata.id, metadata.entry_count
    );

    Ok(())
}

fn list_backup_snapshots(
    path: &Path,
    adapter_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_config(path)?;
    let backup_dir = ensure_backup_dir(path, &mut config)?;
    if !config.backup_enabled {
        return Err("backups are not enabled; run libresync backup configure --enable".into());
    }

    let app_key = config.app_key()?;
    let store = FileSnapshotStore::new(&backup_dir)?;
    let manager = BackupManager::new(app_key, Arc::new(store));

    let snapshots = manager.list_snapshots(adapter_id)?;
    if snapshots.is_empty() {
        println!("No snapshots found for adapter {}", adapter_id);
        return Ok(());
    }

    for snapshot in snapshots {
        println!(
            "{} | {} | entries: {}",
            snapshot.id, snapshot.created_at_unix_secs, snapshot.entry_count
        );
        if let Some(device_id) = snapshot.created_by_device_id.as_deref() {
            println!("  device: {device_id}");
        }
        if let Some(label) = snapshot.label.as_deref() {
            println!("  label: {label}");
        }
        if let Some(note) = snapshot.note {
            println!("  note: {note}");
        }
    }

    Ok(())
}

fn preview_backup_snapshot(
    path: &Path,
    adapter_id: &str,
    snapshot_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_config(path)?;
    let backup_dir = ensure_backup_dir(path, &mut config)?;
    if !config.backup_enabled {
        return Err("backups are not enabled; run libresync backup configure --enable".into());
    }

    let app_key = config.app_key()?;
    let store = FileSnapshotStore::new(&backup_dir)?;
    let manager = BackupManager::new(app_key, Arc::new(store));
    let (adapter, _backup_adapter) = backup_adapter_for_config(&config, adapter_id)?;

    let mut state = load_or_init_state(&config)?;
    adapter.load_into_state(&mut state)?;

    let (_metadata, entries) = manager.load_snapshot_entries(adapter_id, snapshot_id)?;
    let summary = summarize_snapshot_diff(&state, &entries)?;
    print_snapshot_summary(adapter_id, snapshot_id, &summary);

    Ok(())
}

fn restore_backup_snapshot(
    path: &Path,
    adapter_id: &str,
    snapshot_id: &str,
    confirm: bool,
    confirm_id: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_config(path)?;
    let backup_dir = ensure_backup_dir(path, &mut config)?;
    if !config.backup_enabled {
        return Err("backups are not enabled; run libresync backup configure --enable".into());
    }
    if !config.backup_allow_restore {
        return Err(
            "restores are disabled; run libresync backup configure --allow-restore".into(),
        );
    }

    let app_key = config.app_key()?;
    let store = FileSnapshotStore::new(&backup_dir)?;
    let manager = BackupManager::new(app_key, Arc::new(store));
    let (adapter, backup_adapter) = backup_adapter_for_config(&config, adapter_id)?;

    let mut state = load_or_init_state(&config)?;
    adapter.load_into_state(&mut state)?;

    let (_metadata, entries) = manager.load_snapshot_entries(adapter_id, snapshot_id)?;
    let summary = summarize_snapshot_diff(&state, &entries)?;
    print_snapshot_summary(adapter_id, snapshot_id, &summary);

    if !confirm {
        return Err("restore requires --confirm".into());
    }
    if confirm_id
        .as_deref()
        .map(|id| id != snapshot_id)
        .unwrap_or(true)
    {
        return Err("restore requires --confirm-id that matches snapshot_id".into());
    }

    manager.restore_snapshot(&backup_adapter, &mut state, snapshot_id, RestoreOptions::confirmed())?;
    adapter.apply_from_state(&state)?;
    state.save_encrypted(&config.app_key()?, &config.state_path)?;

    println!("Restored snapshot {}", snapshot_id);
    Ok(())
}

fn backup_adapter_for_config(
    config: &Config,
    adapter_id: &str,
) -> Result<(Arc<JsonFileAdapter>, DataAdapterBackup), Box<dyn std::error::Error>> {
    if adapter_id != FILE_KEY {
        return Err("only the file adapter is supported for CLI backups".into());
    }
    let data_path = config
        .data_path
        .clone()
        .ok_or("no file selected; run libresync select")?;
    let adapter = Arc::new(JsonFileAdapter::new(FILE_KEY, data_path));
    let adapter_trait: Arc<dyn DataAdapter> = adapter.clone();
    let backup_adapter = DataAdapterBackup::new(adapter_trait);
    Ok((adapter, backup_adapter))
}

fn ensure_backup_dir(path: &Path, config: &mut Config) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if config.backup_dir.is_none() {
        config.set_backup_dir(default_backup_dir(path));
        save_config(path, config)?;
    }
    Ok(config.backup_dir(path))
}

fn print_snapshot_summary(adapter_id: &str, snapshot_id: &str, summary: &libresync::SnapshotDiffSummary) {
    println!("Snapshot diff for adapter {adapter_id} / {snapshot_id}");
    println!(
        "Entries: snapshot {} | current {}",
        summary.total_snapshot_entries, summary.total_state_entries
    );
    println!(
        "Counts: new {} | changed {} | unchanged {} | missing {}",
        summary.overall.new_entries,
        summary.overall.changed_entries,
        summary.overall.unchanged_entries,
        summary.overall.missing_entries
    );
    if !summary.by_group.is_empty() {
        println!("Groups:");
        for (group, counts) in &summary.by_group {
            println!(
                "  {group}: new {} | changed {} | unchanged {} | missing {}",
                counts.new_entries,
                counts.changed_entries,
                counts.unchanged_entries,
                counts.missing_entries
            );
        }
    }
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
    if let Some(keys) = config.device_keys.as_ref() {
        println!("Your fingerprint:   {}", keys.fingerprint);
    }
    let remote_fingerprint = remote_device
        .fingerprint
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    println!("Remote fingerprint: {}", remote_fingerprint);

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

fn unpair_device(
    path: &Path,
    device_id: &str,
    auto_accept: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_config(path)?;
    if !config.devices.contains_key(device_id) {
        return Err(format!("device not paired: {device_id}").into());
    }

    let confirmed = if auto_accept {
        true
    } else {
        prompt_yes_no(&format!(
            "Revoke pairing with {device_id}? [y/N]: "
        ))
        .unwrap_or(false)
    };

    if !confirmed {
        return Err("unpair aborted locally".into());
    }

    config.devices.remove(device_id);
    save_config(path, &config)?;

    println!("Unpaired {device_id}");
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
    let app_key = config.lock().expect("config lock").app_key()?;
    let state = state.lock().expect("state poisoned");
    state.save_encrypted(&app_key, &config.lock().expect("config lock").state_path)?;
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
    let app_key = config.app_key()?;
    let state = load_or_init_state(&config)?;
    let handler = Arc::new(StaticHandler {
        app_id: config.app_id.clone(),
        allowed: allowed_fingerprint_map(&config),
        keys: device_keys,
        app_key,
    });
    let mut engine = Engine::new(EngineConfig::new(config.identity()), state, handler);
    let adapter = Arc::new(JsonFileAdapter::new(FILE_KEY, data_path));
    let adapter_trait: Arc<dyn DataAdapter> = adapter;
    engine.register_adapter(adapter_trait)?;

    let remote_device = engine.sync_now(device, FILE_KEY)?;

    let state = engine.state();
    let app_key = config.app_key()?;
    state
        .lock()
        .expect("state lock")
        .save_encrypted(&app_key, &config.state_path)?;
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
    if let Some(keys) = config.device_keys.as_ref() {
        println!("Fingerprint: {}", keys.fingerprint);
    }
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
        if let Some(fingerprint) = record.fingerprint.as_deref() {
            println!("    fingerprint: {fingerprint}");
        }
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

    if config.app_key.is_none() {
        let app_key = AppKey::generate()
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error.to_string()))?;
        config.set_app_key(&app_key);
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

fn default_backup_dir(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .map(|parent| parent.join("backups"))
        .unwrap_or_else(|| PathBuf::from("backups"))
}

fn load_or_init_state(config: &Config) -> Result<State, Box<dyn std::error::Error>> {
    let app_key = config.app_key()?;
    if config.state_path.exists() {
        let state = State::load_maybe_encrypted(&app_key, &config.state_path)?;
        if state.device_id != config.device_id {
            return Err("state device id does not match config".into());
        }
        state.save_encrypted(&app_key, &config.state_path)?;
        Ok(state)
    } else {
        let state = State::new(config.device_id.clone());
        state.save_encrypted(&app_key, &config.state_path)?;
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
    let app_key = config.app_key()?;
    let state = load_or_init_state(config)?;
    let handler = Arc::new(StaticHandler {
        app_id: config.app_id.clone(),
        allowed: allowed_fingerprint_map(config),
        keys: device_keys,
        app_key,
    });
    let mut engine = Engine::new(EngineConfig::new(config.identity()), state, handler);
    let adapter = Arc::new(JsonFileAdapter::new(FILE_KEY, data_path.clone()));
    let adapter_trait: Arc<dyn DataAdapter> = adapter;
    engine.register_adapter(adapter_trait)?;

    let remote_device = engine.sync_now(device, FILE_KEY)?;
    let after_bytes = fs::read(&data_path).unwrap_or_default();
    let changed = before_bytes != after_bytes;

    let state = engine.state();
    let app_key = config.app_key()?;
    state
        .lock()
        .expect("state lock")
        .save_encrypted(&app_key, &config.state_path)?;
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
    use std::collections::HashMap;
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener};
    use std::time::Duration;
    use tempfile::tempdir;

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
        let app_key = AppKey::generate().expect("app key");
        let config = Config {
            device_id: "local-device".to_string(),
            app_id: APP_ID_DEFAULT.to_string(),
            user_id: "user".to_string(),
            state_path: PathBuf::from("state.json"),
            data_path: None,
            device_keys: Some(device_keys),
            app_key: Some(BASE64.encode(app_key.as_bytes())),
            backup_enabled: false,
            backup_allow_restore: false,
            backup_dir: None,
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
        let app_key = AppKey::generate().expect("app key");
        let config = Config {
            device_id: "local-device".to_string(),
            app_id: APP_ID_DEFAULT.to_string(),
            user_id: "user".to_string(),
            state_path: PathBuf::from("state.json"),
            data_path: None,
            device_keys: Some(device_keys),
            app_key: Some(BASE64.encode(app_key.as_bytes())),
            backup_enabled: false,
            backup_allow_restore: false,
            backup_dir: None,
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
        let app_key = AppKey::generate().expect("app key");
        let mut config = Config {
            device_id: "local".to_string(),
            app_id: APP_ID_DEFAULT.to_string(),
            user_id: "user".to_string(),
            state_path: PathBuf::from("state.json"),
            data_path: None,
            device_keys: Some(device_keys),
            app_key: Some(BASE64.encode(app_key.as_bytes())),
            backup_enabled: false,
            backup_allow_restore: false,
            backup_dir: None,
            devices: BTreeMap::new(),
        };

        let identity = Identity::new("remote", APP_ID_DEFAULT, "remote-user");
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 52345));
        config.upsert_device(&identity, Some(addr), None);

        let record = config.devices.get("remote").expect("record");
        assert_eq!(record.last_seen_addr.as_deref(), Some("127.0.0.1:52345"));
        assert!(record.last_seen_unix_secs.unwrap_or(0) > 0);
    }

    #[test]
    fn backup_configure_updates_flags() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        configure_backups(
            &config_path,
            true,
            true,
            Some(temp.path().join("custom-backups")),
        )
        .expect("configure");

        let config = load_config(&config_path).expect("load");
        assert!(config.backup_enabled);
        assert!(config.backup_allow_restore);
        assert_eq!(
            config.backup_dir(&config_path),
            temp.path().join("custom-backups")
        );
    }

    #[test]
    fn backup_snapshot_list_restore_round_trip() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let data_path = temp.path().join("data.json");

        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");
        select_file(&config_path, &data_path).expect("select");
        configure_backups(&config_path, true, true, None).expect("configure");
        std::fs::write(&data_path, b"{\"before\":true}").expect("write");

        create_backup_snapshot(&config_path, FILE_KEY, Some("test".to_string()))
            .expect("snapshot");

        let snapshots = {
            let mut config = load_config(&config_path).expect("load");
            let backup_dir = ensure_backup_dir(&config_path, &mut config).expect("dir");
            let app_key = config.app_key().expect("app key");
            let manager = BackupManager::new(
                app_key,
                Arc::new(FileSnapshotStore::new(&backup_dir).expect("store")),
            );
            manager.list_snapshots(FILE_KEY).expect("list")
        };
        assert_eq!(snapshots.len(), 1);

        preview_backup_snapshot(&config_path, FILE_KEY, &snapshots[0].id)
            .expect("preview");

        std::fs::write(&data_path, b"{\"after\":true}").expect("write after");
        restore_backup_snapshot(
            &config_path,
            FILE_KEY,
            &snapshots[0].id,
            true,
            Some(snapshots[0].id.clone()),
        )
            .expect("restore");

        let restored = std::fs::read_to_string(&data_path).expect("read");
        assert!(restored.contains("before"));
    }

    #[test]
    fn backup_restore_requires_allow_restore() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let data_path = temp.path().join("data.json");

        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");
        select_file(&config_path, &data_path).expect("select");
        configure_backups(&config_path, true, false, None).expect("configure");

        let result = restore_backup_snapshot(
            &config_path,
            FILE_KEY,
            "missing",
            true,
            Some("missing".to_string()),
        );
        assert!(result.is_err());
    }

    #[test]
    fn backup_restore_requires_confirm_id() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let data_path = temp.path().join("data.json");

        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");
        select_file(&config_path, &data_path).expect("select");
        configure_backups(&config_path, true, true, None).expect("configure");

        let result = restore_backup_snapshot(&config_path, FILE_KEY, "missing", true, None);
        assert!(result.is_err());
    }

    #[test]
    fn init_config_rejects_existing_without_force() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        std::fs::write(&config_path, b"{}").expect("write");

        let result = init_config(&config_path, APP_ID_DEFAULT, None, None, false);
        assert!(result.is_err());
    }

    #[test]
    fn init_config_creates_state_file() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");

        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        let state_path = default_state_path(&config_path);
        assert!(state_path.exists());
    }

    #[test]
    fn load_config_generates_keys_and_app_key() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let config = Config {
            device_id: "device".to_string(),
            app_id: APP_ID_DEFAULT.to_string(),
            user_id: "user".to_string(),
            state_path: temp.path().join("state.json"),
            data_path: None,
            device_keys: None,
            app_key: None,
            backup_enabled: false,
            backup_allow_restore: false,
            backup_dir: None,
            devices: BTreeMap::new(),
        };
        save_config(&config_path, &config).expect("save");

        let loaded = load_config(&config_path).expect("load");
        assert!(loaded.device_keys.is_some());
        assert!(loaded.app_key.is_some());
    }

    #[test]
    fn config_app_key_missing_errors() {
        let config = Config {
            device_id: "device".to_string(),
            app_id: APP_ID_DEFAULT.to_string(),
            user_id: "user".to_string(),
            state_path: PathBuf::from("state.json"),
            data_path: None,
            device_keys: None,
            app_key: None,
            backup_enabled: false,
            backup_allow_restore: false,
            backup_dir: None,
            devices: BTreeMap::new(),
        };
        assert!(config.app_key().is_err());
    }

    #[test]
    fn config_device_keys_missing_errors() {
        let config = Config {
            device_id: "device".to_string(),
            app_id: APP_ID_DEFAULT.to_string(),
            user_id: "user".to_string(),
            state_path: PathBuf::from("state.json"),
            data_path: None,
            device_keys: None,
            app_key: None,
            backup_enabled: false,
            backup_allow_restore: false,
            backup_dir: None,
            devices: BTreeMap::new(),
        };
        assert!(config.device_keys().is_err());
    }

    #[test]
    fn log_and_pid_paths_use_parent_directory() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let log_path = log_path_for_config(&config_path);
        let pid_path = pid_path_for_config(&config_path);
        assert!(log_path.ends_with("libresync-listen.log"));
        assert!(pid_path.ends_with("libresync-listen.pid"));
    }

    #[test]
    fn ensure_json_file_creates_default() {
        let temp = tempdir().expect("tempdir");
        let data_path = temp.path().join("nested").join("data.json");
        ensure_json_file(&data_path).expect("ensure");
        let contents = std::fs::read_to_string(&data_path).expect("read");
        assert_eq!(contents, "{}");
    }

    #[test]
    fn format_relative_inner_formats_ranges() {
        assert_eq!(format_relative_inner(5, false), "just now");
        assert_eq!(format_relative_inner(30, false), "30s ago");
        assert_eq!(format_relative_inner(90, true), "in 1m");
        assert_eq!(format_relative_inner(3600, false), "1h ago");
    }

    #[test]
    fn print_listener_status_handles_missing_and_invalid_pid() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");

        print_listener_status(&config_path).expect("missing pid");

        let pid_path = pid_path_for_config(&config_path);
        std::fs::write(&pid_path, b"abc").expect("write pid");
        print_listener_status(&config_path).expect("invalid pid");

        std::fs::write(&pid_path, b"999999").expect("write pid");
        print_listener_status(&config_path).expect("stale pid");
    }

    #[test]
    fn status_without_discover_runs() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        let mut config = load_config(&config_path).expect("load");
        config.devices.insert(
            "remote-device".to_string(),
            DeviceRecord {
                device_id: "remote-device".to_string(),
                user_id: "remote-user".to_string(),
                app_id: APP_ID_DEFAULT.to_string(),
                last_seen_addr: Some("127.0.0.1:1".to_string()),
                last_seen_unix_secs: Some(now_unix_secs()),
                fingerprint: Some("fingerprint".to_string()),
            },
        );
        save_config(&config_path, &config).expect("save");

        status(&config_path, false, 0).expect("status");
    }

    #[test]
    fn resolve_addresses_for_watch_dedupes_and_orders() {
        let identity = Identity::new("local-device", APP_ID_DEFAULT, "user");
        let device_keys = DeviceKeysRecord::from_keys(
            &DeviceKeys::generate(&identity).expect("device keys"),
        );
        let app_key = AppKey::generate().expect("app key");
        let mut config = Config {
            device_id: "local-device".to_string(),
            app_id: APP_ID_DEFAULT.to_string(),
            user_id: "user".to_string(),
            state_path: PathBuf::from("state.json"),
            data_path: None,
            device_keys: Some(device_keys),
            app_key: Some(BASE64.encode(app_key.as_bytes())),
            backup_enabled: false,
            backup_allow_restore: false,
            backup_dir: None,
            devices: BTreeMap::new(),
        };
        config.devices.insert(
            "remote-device".to_string(),
            DeviceRecord {
                device_id: "remote-device".to_string(),
                user_id: "remote-user".to_string(),
                app_id: APP_ID_DEFAULT.to_string(),
                last_seen_addr: Some("127.0.0.1:1234".to_string()),
                last_seen_unix_secs: None,
                fingerprint: None,
            },
        );

        let addr: SocketAddr = "127.0.0.1:1234".parse().expect("addr");
        let mut discovered = HashMap::new();
        discovered.insert("remote-device".to_string(), addr);
        let addresses = resolve_addresses_for_watch(&config, "remote-device", &discovered);
        assert_eq!(addresses, vec![addr]);

        let alt: SocketAddr = "127.0.0.1:2345".parse().expect("addr");
        discovered.insert("remote-device".to_string(), alt);
        let addresses = resolve_addresses_for_watch(&config, "remote-device", &discovered);
        assert_eq!(addresses, vec![alt, addr]);
    }

    #[test]
    fn is_port_listening_detects_open_and_closed_ports() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        assert!(is_port_listening(addr).expect("listening"));

        drop(listener);
        std::thread::sleep(Duration::from_millis(30));
        assert!(!is_port_listening(addr).expect("not listening"));
    }

    #[test]
    fn ensure_listener_running_returns_ok_when_active() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let listen = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port));
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");

        ensure_listener_running(&config_path, listen).expect("ensure");
        drop(listener);
    }

    #[test]
    fn load_or_init_state_creates_and_validates() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");
        let config = load_config(&config_path).expect("load");

        std::fs::remove_file(&config.state_path).expect("remove state");
        let state = load_or_init_state(&config).expect("state");
        assert_eq!(state.device_id, config.device_id);
        assert!(config.state_path.exists());

        let bad_state = State::new("other-device");
        bad_state
            .save_encrypted(&config.app_key().expect("app key"), &config.state_path)
            .expect("save");
        let result = load_or_init_state(&config);
        assert!(result.is_err());
    }

    #[test]
    fn report_error_prints_hint_and_verbose() {
        let error = io::Error::new(io::ErrorKind::Other, "boom");
        report_error(&error, false);
        report_error(&error, true);
    }

    #[test]
    fn run_init_select_status_stop() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let data_path = temp.path().join("data.json");

        run(Cli {
            verbose: false,
            command: Commands::Init {
                config: Some(config_path.clone()),
                app_id: APP_ID_DEFAULT.to_string(),
                device_id: None,
                user_id: None,
                force: true,
            },
        })
        .expect("run init");

        run(Cli {
            verbose: false,
            command: Commands::Select {
                config: Some(config_path.clone()),
                file: data_path.clone(),
            },
        })
        .expect("run select");

        run(Cli {
            verbose: false,
            command: Commands::Status {
                config: Some(config_path.clone()),
                no_discover: true,
                timeout_secs: 0,
            },
        })
        .expect("run status");

        let pid_path = pid_path_for_config(&config_path);
        std::fs::write(&pid_path, b"999999").expect("write pid");

        run(Cli {
            verbose: false,
            command: Commands::Stop {
                config: Some(config_path),
            },
        })
        .expect("run stop");
    }

    #[test]
    fn run_backup_list_without_snapshots() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let data_path = temp.path().join("data.json");

        run(Cli {
            verbose: false,
            command: Commands::Init {
                config: Some(config_path.clone()),
                app_id: APP_ID_DEFAULT.to_string(),
                device_id: None,
                user_id: None,
                force: true,
            },
        })
        .expect("run init");

        run(Cli {
            verbose: false,
            command: Commands::Select {
                config: Some(config_path.clone()),
                file: data_path,
            },
        })
        .expect("run select");

        run(Cli {
            verbose: false,
            command: Commands::Backup {
                command: BackupCommands::Configure {
                    config: Some(config_path.clone()),
                    enable: true,
                    allow_restore: false,
                    dir: None,
                },
            },
        })
        .expect("run backup configure");

        run(Cli {
            verbose: false,
            command: Commands::Backup {
                command: BackupCommands::List {
                    config: Some(config_path),
                    adapter_id: FILE_KEY.to_string(),
                },
            },
        })
        .expect("run backup list");
    }

    #[test]
    fn resolve_config_path_prefers_override() {
        let path = PathBuf::from("override.json");
        let resolved = resolve_config_path(Some(path.clone()));
        assert_eq!(resolved, path);
    }

    #[test]
    fn default_paths_helpers_use_parent() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let state_path = default_state_path(&config_path);
        let backup_dir = default_backup_dir(&config_path);
        assert!(state_path.ends_with("config.state.json"));
        assert!(backup_dir.ends_with("backups"));
    }

    #[test]
    fn configure_backups_no_changes_errors() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        let result = configure_backups(&config_path, false, false, None);
        assert!(result.is_err());
    }

    #[test]
    fn backup_adapter_for_config_errors_on_unknown_id() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");
        let config = load_config(&config_path).expect("load");

        let result = backup_adapter_for_config(&config, "unknown");
        assert!(result.is_err());
    }

    #[test]
    fn allowed_fingerprint_map_filters_missing() {
        let identity = Identity::new("local-device", APP_ID_DEFAULT, "user");
        let device_keys = DeviceKeysRecord::from_keys(
            &DeviceKeys::generate(&identity).expect("device keys"),
        );
        let app_key = AppKey::generate().expect("app key");
        let mut config = Config {
            device_id: "local-device".to_string(),
            app_id: APP_ID_DEFAULT.to_string(),
            user_id: "user".to_string(),
            state_path: PathBuf::from("state.json"),
            data_path: None,
            device_keys: Some(device_keys),
            app_key: Some(BASE64.encode(app_key.as_bytes())),
            backup_enabled: false,
            backup_allow_restore: false,
            backup_dir: None,
            devices: BTreeMap::new(),
        };
        config.devices.insert(
            "device-a".to_string(),
            DeviceRecord {
                device_id: "device-a".to_string(),
                user_id: "user-a".to_string(),
                app_id: APP_ID_DEFAULT.to_string(),
                last_seen_addr: None,
                last_seen_unix_secs: None,
                fingerprint: Some("finger-a".to_string()),
            },
        );
        config.devices.insert(
            "device-b".to_string(),
            DeviceRecord {
                device_id: "device-b".to_string(),
                user_id: "user-b".to_string(),
                app_id: APP_ID_DEFAULT.to_string(),
                last_seen_addr: None,
                last_seen_unix_secs: None,
                fingerprint: None,
            },
        );

        let map = allowed_fingerprint_map(&config);
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("device-a").map(String::as_str), Some("finger-a"));
    }

    #[test]
    fn unpair_device_removes_and_errors_when_missing() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        let mut config = load_config(&config_path).expect("load");
        config.devices.insert(
            "remote-device".to_string(),
            DeviceRecord {
                device_id: "remote-device".to_string(),
                user_id: "remote-user".to_string(),
                app_id: APP_ID_DEFAULT.to_string(),
                last_seen_addr: None,
                last_seen_unix_secs: None,
                fingerprint: None,
            },
        );
        save_config(&config_path, &config).expect("save");

        unpair_device(&config_path, "remote-device", true).expect("unpair");
        let config = load_config(&config_path).expect("reload");
        assert!(!config.devices.contains_key("remote-device"));

        let result = unpair_device(&config_path, "remote-device", true);
        assert!(result.is_err());
    }

    #[test]
    fn format_last_seen_includes_prefix() {
        let formatted = format_last_seen(now_unix_secs());
        assert!(formatted.contains("last seen:"));
    }

    #[test]
    fn default_config_path_ends_with_filename() {
        let path = default_config_path();
        assert!(path
            .file_name()
            .map(|name| name == CONFIG_FILE_NAME)
            .unwrap_or(false));
    }

    #[test]
    fn resolve_config_path_none_uses_default() {
        let resolved = resolve_config_path(None);
        assert_eq!(resolved, default_config_path());
    }

    #[test]
    fn load_config_invalid_json_errors() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        std::fs::write(&config_path, b"not-json").expect("write");
        let result = load_config(&config_path);
        assert!(result.is_err());
    }

    #[test]
    fn save_config_creates_parent_directory() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("nested").join("config.json");
        let config = Config {
            device_id: "device".to_string(),
            app_id: APP_ID_DEFAULT.to_string(),
            user_id: "user".to_string(),
            state_path: temp.path().join("state.json"),
            data_path: None,
            device_keys: None,
            app_key: None,
            backup_enabled: false,
            backup_allow_restore: false,
            backup_dir: None,
            devices: BTreeMap::new(),
        };

        save_config(&config_path, &config).expect("save");
        assert!(config_path.exists());
    }

    #[test]
    fn select_file_creates_data_file() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let data_path = temp.path().join("data.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");
        select_file(&config_path, &data_path).expect("select");
        assert!(data_path.exists());
    }

    #[test]
    fn engine_for_config_uses_identity() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");
        let config = load_config(&config_path).expect("load");

        let engine = engine_for_config(&config);
        assert_eq!(engine.identity().device_id, config.device_id);
    }

    #[test]
    fn backup_list_errors_when_disabled() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        let result = list_backup_snapshots(&config_path, FILE_KEY);
        assert!(result.is_err());
    }

    #[test]
    fn create_backup_snapshot_errors_when_disabled() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let data_path = temp.path().join("data.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");
        select_file(&config_path, &data_path).expect("select");

        let result = create_backup_snapshot(&config_path, FILE_KEY, None);
        assert!(result.is_err());
    }

    #[test]
    fn restore_backup_snapshot_errors_when_disabled() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        let result = restore_backup_snapshot(
            &config_path,
            FILE_KEY,
            "missing",
            true,
            Some("missing".to_string()),
        );
        assert!(result.is_err());
    }

    #[test]
    fn backup_adapter_for_config_requires_selected_file() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");
        let config = load_config(&config_path).expect("load");

        let result = backup_adapter_for_config(&config, FILE_KEY);
        assert!(result.is_err());
    }

    #[test]
    fn ensure_backup_dir_sets_default_when_missing() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");
        let mut config = load_config(&config_path).expect("load");

        let dir = ensure_backup_dir(&config_path, &mut config).expect("ensure");
        assert_eq!(dir, default_backup_dir(&config_path));
        assert!(config.backup_dir.is_some());
    }

    #[test]
    fn print_snapshot_summary_accepts_empty_summary() {
        let summary = libresync::SnapshotDiffSummary::default();
        print_snapshot_summary("file", "snapshot", &summary);
    }

    #[test]
    fn refresh_with_address_errors_without_selected_file() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");
        let mut config = load_config(&config_path).expect("load");

        let addr: SocketAddr = "127.0.0.1:1".parse().expect("addr");
        let result = refresh_with_address(&config_path, &mut config, addr);
        assert!(result.is_err());
    }

    #[test]
    fn refresh_all_devices_handles_connection_errors() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let data_path = temp.path().join("data.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");
        select_file(&config_path, &data_path).expect("select");

        let mut config = load_config(&config_path).expect("load");
        config.devices.insert(
            "remote-device".to_string(),
            DeviceRecord {
                device_id: "remote-device".to_string(),
                user_id: "remote-user".to_string(),
                app_id: APP_ID_DEFAULT.to_string(),
                last_seen_addr: Some("127.0.0.1:1".to_string()),
                last_seen_unix_secs: None,
                fingerprint: None,
            },
        );

        let changed = refresh_all_devices(&config_path, &mut config, false).expect("refresh");
        assert!(!changed);
    }

    #[test]
    fn stop_listener_errors_on_non_libresync_pid() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        let pid_path = pid_path_for_config(&config_path);
        std::fs::write(&pid_path, b"1").expect("write pid");
        let result = stop_listener(&config_path);
        assert!(result.is_err());
    }

    #[test]
    fn run_unpair_command() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        let mut config = load_config(&config_path).expect("load");
        config.devices.insert(
            "remote-device".to_string(),
            DeviceRecord {
                device_id: "remote-device".to_string(),
                user_id: "remote-user".to_string(),
                app_id: APP_ID_DEFAULT.to_string(),
                last_seen_addr: None,
                last_seen_unix_secs: None,
                fingerprint: None,
            },
        );
        save_config(&config_path, &config).expect("save");

        run(Cli {
            verbose: false,
            command: Commands::Unpair {
                config: Some(config_path.clone()),
                device_id: "remote-device".to_string(),
                yes: true,
            },
        })
        .expect("run unpair");

        let config = load_config(&config_path).expect("load");
        assert!(!config.devices.contains_key("remote-device"));
    }

}
