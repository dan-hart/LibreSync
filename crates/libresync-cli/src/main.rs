use std::collections::{BTreeMap, HashMap, HashSet};
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
use libresync::{sync_with_device, DeviceHandler, Identity, State, SyncListener};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, System};

const APP_ID_DEFAULT: &str = "com.codedbydan.libresync-cli";
const SERVICE_TYPE: &str = "_libresync._tcp.local.";
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
    devices: BTreeMap<String, DeviceRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceRecord {
    device_id: String,
    user_id: String,
    app_id: String,
    last_seen_addr: Option<String>,
    #[serde(default)]
    last_seen_unix_secs: Option<u64>,
}

impl Config {
    fn identity(&self) -> Identity {
        Identity::new(&self.device_id, &self.app_id, &self.user_id)
    }

    fn is_paired(&self, device_id: &str) -> bool {
        self.devices.contains_key(device_id)
    }

    fn upsert_device(&mut self, identity: &Identity, addr: Option<SocketAddr>) {
        let entry = self
            .devices
            .entry(identity.device_id.clone())
            .or_insert(DeviceRecord {
            device_id: identity.device_id.clone(),
            user_id: identity.user_id.clone(),
            app_id: identity.app_id.clone(),
            last_seen_addr: None,
            last_seen_unix_secs: None,
        });
        entry.user_id = identity.user_id.clone();
        entry.app_id = identity.app_id.clone();
        if let Some(addr) = addr {
            entry.last_seen_addr = Some(addr.to_string());
            entry.last_seen_unix_secs = Some(now_unix_secs());
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
            config.upsert_device(identity, None);
            save_config(&self.config_path, &config)
                .map_err(|error| libresync::Error::Protocol(error.to_string()))?;
        }

        Ok(accepted)
    }
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
            interval_secs,
            debounce_ms,
            no_discover,
        } => {
            let config = resolve_config_path(config);
            watch_file(&config, interval_secs, debounce_ms, no_discover)?;
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

    let config = Config {
        device_id,
        app_id: app_id.to_string(),
        user_id,
        state_path,
        data_path: None,
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
    let devices = browse_mdns(&config.app_id, Duration::from_secs(timeout_secs))?;
    let devices = filter_out_local(devices, &config.device_id);

    if devices.is_empty() {
        println!("No devices found for app {}", config.app_id);
        return Ok(());
    }

    for device in devices {
        println!(
            "{} ({}) at {}",
            device.device_id, device.user_id, device.address
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
    let local_identity = config.identity();
    let device = resolve_device_address(
        &config.app_id,
        &config.device_id,
        device,
        device_id.as_deref(),
    )?;

    let stream = std::net::TcpStream::connect_timeout(&device, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    let mut reader = std::io::BufReader::new(stream.try_clone()?);
    let mut writer = std::io::BufWriter::new(stream);

    libresync::write_message(
        &mut writer,
        &libresync::Message::PairRequest {
            identity: local_identity,
        },
    )?;

    let response = libresync::read_message(&mut reader)?;
    let (remote_identity, accepted) = match response {
        libresync::Message::PairResponse { identity, accepted } => (identity, accepted),
        _ => return Err("unexpected pairing response".into()),
    };

    if !accepted {
        return Err(format!("pairing rejected by {}", remote_identity.device_id).into());
    }

    if remote_identity.app_id != config.app_id {
        return Err("app id mismatch during pairing".into());
    }

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

    config.upsert_device(&remote_identity, Some(device));
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
    let state = Arc::new(Mutex::new(state));

    let handler = Arc::new(ConfigHandler {
        app_id: identity.app_id.clone(),
        config_path: path.to_path_buf(),
        config: Arc::clone(&config),
        auto_accept,
    });

    let listener = SyncListener::start(listen, identity.clone(), Arc::clone(&state), handler)?;

    let mdns = if no_discovery {
        None
    } else {
        Some(register_mdns(&identity, listener.addr())?)
    };

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    ctrlc::set_handler(move || {
        running_clone.store(false, Ordering::SeqCst);
    })?;

    println!(
        "Listening on {} (device: {}, user: {})",
        listener.addr(),
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

    let listener = listener.shutdown();
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
    let device = resolve_device_address(
        &config.app_id,
        &config.device_id,
        device,
        device_id.as_deref(),
    )?;
    let data_path = config
        .data_path
        .clone()
        .ok_or("no file selected; run libresync select")?;

    ensure_json_file(&data_path)?;

    let mut state = load_or_init_state(&config)?;

    let local_bytes = fs::read(&data_path)?;
    let should_update = match state.get(FILE_KEY) {
        Some(existing) => existing != local_bytes.as_slice(),
        None => true,
    };
    if should_update {
        state.set(FILE_KEY.to_string(), local_bytes.clone());
    }

    let app_id = config.app_id.clone();
    let allowed: HashSet<String> = config.devices.keys().cloned().collect();
    let device_check = |identity: &Identity| -> libresync::Result<()> {
        if identity.app_id != app_id {
            return Err(libresync::Error::Protocol("app id mismatch".to_string()));
        }
        if !allowed.contains(&identity.device_id) {
            return Err(libresync::Error::Protocol("device not paired".to_string()));
        }
        Ok(())
    };

    let remote_identity = sync_with_device(&config.identity(), &mut state, device, device_check)?;

    if let Some(bytes) = state.get(FILE_KEY) {
        fs::write(&data_path, bytes)?;
    }

    state.save(&config.state_path)?;
    config.upsert_device(&remote_identity, Some(device));
    save_config(path, &config)?;

    println!(
        "Refreshed with {} ({})",
        remote_identity.device_id, remote_identity.user_id
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

    let mut discovered: Vec<DiscoveredDevice> = Vec::new();
    if discover {
        discovered = browse_mdns(&config.app_id, Duration::from_secs(timeout_secs))?;
        discovered = filter_out_local(discovered, &config.device_id);
    }

    let discovered_map: HashMap<String, DiscoveredDevice> = discovered
        .iter()
        .cloned()
        .map(|device| (device.device_id.clone(), device))
        .collect();

    if discover {
        println!("\nDiscovered now:");
        if discovered_map.is_empty() {
            println!("  (none)");
        } else {
            for device in discovered_map.values() {
                let paired = config.devices.contains_key(&device.device_id);
                let status = if paired { "paired" } else { "unpaired" };
                println!(
                    "  {} ({}) at {} [{}]",
                    device.device_id, device.user_id, device.address, status
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
            record.last_seen_addr = Some(found.address.to_string());
            record.last_seen_unix_secs = Some(now_unix_secs());
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
    let config = serde_json::from_slice(&data).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to parse config {}: {error}", path.display()),
        )
    })?;
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

fn register_mdns(
    identity: &Identity,
    listen: SocketAddr,
) -> Result<ServiceDaemon, Box<dyn std::error::Error>> {
    let mdns = ServiceDaemon::new()?;
    let mut properties = HashMap::new();
    properties.insert("app_id".to_string(), identity.app_id.clone());
    properties.insert("device_id".to_string(), identity.device_id.clone());
    properties.insert("user_id".to_string(), identity.user_id.clone());

    let ips = local_ips(listen.ip())?;
    let host_name = format!("{}.local.", identity.device_id);
    let service = ServiceInfo::new(
        SERVICE_TYPE,
        &identity.device_id,
        &host_name,
        ips.as_slice(),
        listen.port(),
        properties,
    )?;

    mdns.register(service)?;
    Ok(mdns)
}

#[derive(Clone, Debug)]
struct DiscoveredDevice {
    device_id: String,
    user_id: String,
    address: SocketAddr,
}

fn filter_out_local(
    devices: Vec<DiscoveredDevice>,
    local_device_id: &str,
) -> Vec<DiscoveredDevice> {
    devices
        .into_iter()
        .filter(|device| device.device_id != local_device_id)
        .collect()
}

fn browse_mdns(app_id: &str, timeout: Duration) -> Result<Vec<DiscoveredDevice>, Box<dyn std::error::Error>> {
    let mdns = ServiceDaemon::new()?;
    let receiver = mdns.browse(SERVICE_TYPE)?;
    let start = Instant::now();
    let mut devices: BTreeMap<String, DiscoveredDevice> = BTreeMap::new();

    while start.elapsed() < timeout {
        let remaining = timeout.saturating_sub(start.elapsed());
        let event = receiver.recv_timeout(remaining.min(Duration::from_millis(200)));
        let event = match event {
            Ok(event) => event,
            Err(flume::RecvTimeoutError::Timeout) => continue,
            Err(error) => return Err(error.into()),
        };

        if let ServiceEvent::ServiceResolved(info) = event {
            if let Some(device) = parse_service_info(&info, app_id) {
                devices.insert(device.device_id.clone(), device);
            }
        }
    }

    mdns.shutdown()?;
    Ok(devices.into_values().collect())
}

fn parse_service_info(info: &ServiceInfo, expected_app_id: &str) -> Option<DiscoveredDevice> {
    let properties = info.get_properties();
    let app_id = properties.get("app_id")?.val_str();
    if app_id != expected_app_id {
        return None;
    }
    let device_id = properties.get("device_id")?.val_str();
    let user_id = properties.get("user_id")?.val_str();
    let address = pick_address(info, info.get_port())?;

    Some(DiscoveredDevice {
        device_id: device_id.to_string(),
        user_id: user_id.to_string(),
        address,
    })
}

fn resolve_device_address(
    app_id: &str,
    local_device_id: &str,
    device: Option<SocketAddr>,
    device_id: Option<&str>,
) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    resolve_device_address_with_timeout(
        app_id,
        local_device_id,
        device,
        device_id,
        Duration::from_secs(3),
    )
}

fn resolve_device_address_with_timeout(
    app_id: &str,
    local_device_id: &str,
    device: Option<SocketAddr>,
    device_id: Option<&str>,
    timeout: Duration,
) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    if let Some(device) = device {
        return Ok(device);
    }

    let devices = browse_mdns(app_id, timeout)?;
    let devices = filter_out_local(devices, local_device_id);
    if devices.is_empty() {
        return Err("no devices found on the LAN".into());
    }

    if let Some(device_id) = device_id {
        let matched = devices
            .into_iter()
            .find(|device| device.device_id == device_id)
            .ok_or_else(|| format!("device not found: {device_id}"))?;
        return Ok(matched.address);
    }

    if devices.len() == 1 {
        return Ok(devices[0].address);
    }

    let selection = prompt_select_device(&devices)?;
    Ok(selection.address)
}

fn prompt_select_device(devices: &[DiscoveredDevice]) -> Result<DiscoveredDevice, io::Error> {
    println!("Select a device:");
    for (index, device) in devices.iter().enumerate() {
        println!(
            "  {}) {} ({}) at {}",
            index + 1,
            device.device_id,
            device.user_id,
            device.address
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
    interval_secs: u64,
    debounce_ms: u64,
    no_discover: bool,
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
            let refresh_result =
                refresh_all_devices(path, &mut config, !no_discover);
            match refresh_result {
                Ok(changed) => {
                    if changed {
                        println!("Refreshed with paired devices.");
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

fn refresh_all_devices(
    path: &Path,
    config: &mut Config,
    discover: bool,
) -> Result<bool, Box<dyn std::error::Error>> {
    if config.devices.is_empty() {
        return Ok(false);
    }

    let mut any_changed = false;
    let device_ids: Vec<String> = config.devices.keys().cloned().collect();
    for device_id in device_ids {
        let addr = resolve_device_for_watch(config, &device_id, discover)?;
        let Some(addr) = addr else {
            continue;
        };
        let refreshed = refresh_with_address(path, config, addr)?;
        if refreshed {
            any_changed = true;
        }
    }

    Ok(any_changed)
}

fn resolve_device_for_watch(
    config: &Config,
    device_id: &str,
    discover: bool,
) -> Result<Option<SocketAddr>, Box<dyn std::error::Error>> {
    if let Some(record) = config.devices.get(device_id) {
        if let Some(addr) = record.last_seen_addr.as_deref() {
            if let Ok(parsed) = addr.parse::<SocketAddr>() {
                return Ok(Some(parsed));
            }
        }
    }

    if !discover {
        return Ok(None);
    }

    let addr = resolve_device_address_with_timeout(
        &config.app_id,
        &config.device_id,
        None,
        Some(device_id),
        Duration::from_secs(2),
    )
    .ok();
    Ok(addr)
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
    ensure_json_file(&data_path)?;

    let mut state = load_or_init_state(config)?;
    let local_bytes = fs::read(&data_path)?;
    let should_update = match state.get(FILE_KEY) {
        Some(existing) => existing != local_bytes.as_slice(),
        None => true,
    };
    if should_update {
        state.set(FILE_KEY.to_string(), local_bytes.clone());
    }

    let app_id = config.app_id.clone();
    let allowed: HashSet<String> = config.devices.keys().cloned().collect();
    let device_check = |identity: &Identity| -> libresync::Result<()> {
        if identity.app_id != app_id {
            return Err(libresync::Error::Protocol("app id mismatch".to_string()));
        }
        if !allowed.contains(&identity.device_id) {
            return Err(libresync::Error::Protocol("device not paired".to_string()));
        }
        Ok(())
    };

    let remote_identity = sync_with_device(&config.identity(), &mut state, device, device_check)?;
    let mut changed = false;
    if let Some(bytes) = state.get(FILE_KEY) {
        if bytes != local_bytes.as_slice() {
            fs::write(&data_path, bytes)?;
            changed = true;
        }
    }

    state.save(&config.state_path)?;
    config.upsert_device(&remote_identity, Some(device));
    save_config(path, config)?;
    Ok(changed)
}

fn pick_address(info: &ServiceInfo, port: u16) -> Option<SocketAddr> {
    let addresses = info.get_addresses();
    if let Some(ip) = addresses.iter().find_map(|addr| match addr {
        IpAddr::V4(ip) => Some(IpAddr::V4(*ip)),
        _ => None,
    }) {
        return Some(SocketAddr::new(ip, port));
    }

    if let Some(ip) = addresses.iter().find_map(|addr| match addr {
        IpAddr::V6(ip) if !ip.is_unicast_link_local() => Some(IpAddr::V6(*ip)),
        _ => None,
    }) {
        return Some(SocketAddr::new(ip, port));
    }

    let ip = addresses.iter().find_map(|addr| match addr {
        IpAddr::V6(ip) => Some(IpAddr::V6(*ip)),
        _ => None,
    })?;
    Some(SocketAddr::new(ip, port))
}

fn local_ips(listen_ip: IpAddr) -> Result<Vec<IpAddr>, Box<dyn std::error::Error>> {
    if !listen_ip.is_unspecified() {
        return Ok(vec![listen_ip]);
    }

    let mut ips = Vec::new();
    for iface in if_addrs::get_if_addrs()? {
        if iface.ip().is_loopback() {
            continue;
        }
        ips.push(iface.ip());
    }

    if ips.is_empty() {
        ips.push(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    }

    Ok(ips)
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
    use std::net::Ipv6Addr;
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
    fn parse_service_info_accepts_matching_app() {
        let mut properties = HashMap::new();
        properties.insert("app_id".to_string(), APP_ID_DEFAULT.to_string());
        properties.insert("device_id".to_string(), "test-device".to_string());
        properties.insert("user_id".to_string(), "test-user".to_string());

        let service = ServiceInfo::new(
            SERVICE_TYPE,
            "test-device",
            "test-device.local.",
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            9000,
            properties,
        )
        .expect("service");

        let device = parse_service_info(&service, APP_ID_DEFAULT).expect("device");
        assert_eq!(device.device_id, "test-device");
    }

    #[test]
    fn pick_address_prefers_ipv4_over_ipv6() {
        let mut properties = HashMap::new();
        properties.insert("app_id".to_string(), APP_ID_DEFAULT.to_string());
        properties.insert("device_id".to_string(), "test-device".to_string());
        properties.insert("user_id".to_string(), "test-user".to_string());

        let addresses = vec![
            IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 22)),
        ];

        let service = ServiceInfo::new(
            SERVICE_TYPE,
            "test-device",
            "test-device.local.",
            addresses.as_slice(),
            9000,
            properties,
        )
        .expect("service");

        let picked = pick_address(&service, 9000).expect("address");
        assert_eq!(
            picked,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 22)), 9000)
        );
    }

    #[test]
    fn pick_address_skips_link_local_ipv6_when_possible() {
        let mut properties = HashMap::new();
        properties.insert("app_id".to_string(), APP_ID_DEFAULT.to_string());
        properties.insert("device_id".to_string(), "test-device".to_string());
        properties.insert("user_id".to_string(), "test-user".to_string());

        let addresses = vec![
            IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 2)),
            IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 3)),
        ];

        let service = ServiceInfo::new(
            SERVICE_TYPE,
            "test-device",
            "test-device.local.",
            addresses.as_slice(),
            9001,
            properties,
        )
        .expect("service");

        let picked = pick_address(&service, 9001).expect("address");
        assert_eq!(
            picked,
            SocketAddr::new(
                IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 3)),
                9001
            )
        );
    }

    #[test]
    fn pick_address_uses_link_local_ipv6_if_only_option() {
        let mut properties = HashMap::new();
        properties.insert("app_id".to_string(), APP_ID_DEFAULT.to_string());
        properties.insert("device_id".to_string(), "test-device".to_string());
        properties.insert("user_id".to_string(), "test-user".to_string());

        let addresses = vec![IpAddr::V6(Ipv6Addr::new(
            0xfe80, 0, 0, 0, 0, 0, 0, 5,
        ))];

        let service = ServiceInfo::new(
            SERVICE_TYPE,
            "test-device",
            "test-device.local.",
            addresses.as_slice(),
            9002,
            properties,
        )
        .expect("service");

        let picked = pick_address(&service, 9002).expect("address");
        assert_eq!(
            picked,
            SocketAddr::new(
                IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 5)),
                9002
            )
        );
    }

    #[test]
    fn browse_mdns_returns_empty_for_short_timeout() {
        let devices = browse_mdns(APP_ID_DEFAULT, Duration::from_millis(10)).expect("browse");
        assert!(devices.is_empty() || devices.iter().all(|p| p.device_id.len() > 0));
    }

    #[test]
    fn resolve_device_address_prefers_explicit_device() {
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 52345));
        let resolved = resolve_device_address_with_timeout(
            APP_ID_DEFAULT,
            "local-device",
            Some(addr),
            None,
            Duration::from_millis(1),
        )
        .expect("resolve");
        assert_eq!(resolved, addr);
    }

    #[test]
    fn resolve_device_address_errors_when_none_found() {
        let error = resolve_device_address_with_timeout(
            APP_ID_DEFAULT,
            "local-device",
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
        let mut config = Config {
            device_id: "local".to_string(),
            app_id: APP_ID_DEFAULT.to_string(),
            user_id: "user".to_string(),
            state_path: PathBuf::from("state.json"),
            data_path: None,
            devices: BTreeMap::new(),
        };

        let identity = Identity::new("remote", APP_ID_DEFAULT, "remote-user");
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 52345));
        config.upsert_device(&identity, Some(addr));

        let record = config.devices.get("remote").expect("record");
        assert_eq!(record.last_seen_addr.as_deref(), Some("127.0.0.1:52345"));
        assert!(record.last_seen_unix_secs.unwrap_or(0) > 0);
    }

    #[test]
    fn local_ips_returns_specific_address() {
        let addr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50));
        let ips = local_ips(addr).expect("ips");
        assert_eq!(ips, vec![addr]);
    }
}
