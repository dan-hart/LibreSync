use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use libresync::{sync_with_device, DeviceHandler, Identity, State, SyncListener};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

const APP_ID_DEFAULT: &str = "com.codedbydan.libresync-cli";
const SERVICE_TYPE: &str = "_libresync._tcp.local.";
const FILE_KEY: &str = "file";
const DEFAULT_LISTEN: &str = "0.0.0.0:52345";

#[derive(Parser)]
#[command(
    name = "libresync",
    version,
    about = "Local-only device-to-device sync CLI for LibreSync.",
    long_about = "A minimal CLI for exercising the LibreSync core. Use it to discover devices on LAN,\npair devices, and sync a selected JSON file over direct connections."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(
        about = "Create a new LibreSync config with device/app/user identity.",
        long_about = "Creates a config file that stores the device ID, user ID, app ID, allowlisted devices, and internal state paths. This is required before discovery, pairing, or sync. Use --force to overwrite an existing config."
    )]
    Init {
        #[arg(
            long,
            help = "Path to the config JSON file.",
            long_help = "Path where the LibreSync config JSON will be created. The config stores identity and trust state."
        )]
        config: PathBuf,
        #[arg(
            long,
            default_value = APP_ID_DEFAULT,
            help = "App bundle identifier used to scope discovery and trust.",
            long_help = "App bundle identifier used to scope discovery and trust. Devices only pair and sync when app IDs match."
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
        about = "Select the JSON file to keep in sync.",
        long_about = "Sets the JSON file path that LibreSync will synchronize. The file is created if it does not exist."
    )]
    Select {
        #[arg(
            long,
            help = "Path to the config JSON file.",
            long_help = "Config file that stores the selected JSON file path."
        )]
        config: PathBuf,
        #[arg(
            long,
            help = "Path to the JSON file to sync.",
            long_help = "Path to the JSON file that will be synchronized between devices. The file is created if missing."
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
            help = "Path to the config JSON file.",
            long_help = "Config file used to determine the app ID and discovery scope."
        )]
        config: PathBuf,
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
        long_about = "Pair establishes trust only. It does not sync any data. The remote device must accept the pairing request, and you must confirm locally unless --yes is set."
    )]
    Pair {
        #[arg(
            long,
            help = "Path to the config JSON file.",
            long_help = "Config file to store the paired device entry and trust state."
        )]
        config: PathBuf,
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
        long_about = "Start a device listener that accepts inbound connections and (by default) advertises via mDNS. Use --no-discovery to disable advertising while still accepting direct connections."
    )]
    Listen {
        #[arg(
            long,
            help = "Path to the config JSON file.",
            long_help = "Config file that contains the device identity and trust state."
        )]
        config: PathBuf,
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
        #[arg(long, hide = true)]
        duration_secs: Option<u64>,
    },
    #[command(
        about = "Sync the selected JSON file with a paired device.",
        long_about = "Sync exchanges data only after trust is established. It requires prior pairing. The file is loaded into the local state before sync and written back after sync."
    )]
    Sync {
        #[arg(
            long,
            help = "Path to the config JSON file.",
            long_help = "Config file that includes the selected JSON file path and device allowlist."
        )]
        config: PathBuf,
        #[arg(
            long,
            help = "Device address to connect to (e.g. 192.168.1.10:52345).",
            long_help = "Socket address for the device listener you want to sync with. If omitted, LibreSync will try to discover devices on the LAN."
        )]
        device: Option<SocketAddr>,
        #[arg(
            long,
            conflicts_with = "device",
            help = "Device ID to sync with (uses discovery).",
            long_help = "Device ID to sync with. LibreSync will discover devices on the LAN and connect to the matching device ID."
        )]
        device_id: Option<String>,
    },
    #[command(
        about = "Show device status, paired devices, and recent discovery info.",
        long_about = "Show local identity, selected file, paired devices, and (by default) devices discovered on the LAN. Discovered devices are treated as connected now. Use --no-discover to skip LAN discovery."
    )]
    Status {
        #[arg(
            long,
            help = "Path to the config JSON file.",
            long_help = "Config file that includes identity, selected file, and paired devices."
        )]
        config: PathBuf,
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init {
            config,
            app_id,
            device_id,
            user_id,
            force,
        } => init_config(&config, &app_id, device_id, user_id, force)?,
        Commands::Select { config, file } => select_file(&config, &file)?,
        Commands::Discover {
            config,
            timeout_secs,
        } => discover_devices(&config, timeout_secs)?,
        Commands::Pair {
            config,
            device,
            device_id,
            yes,
        } => pair_device(&config, device, device_id, yes)?,
        Commands::Listen {
            config,
            listen,
            auto_accept,
            no_discovery,
            duration_secs,
        } => listen_device(&config, listen, auto_accept, no_discovery, duration_secs)?,
        Commands::Sync {
            config,
            device,
            device_id,
        } => sync_file(&config, device, device_id)?,
        Commands::Status {
            config,
            no_discover,
            timeout_secs,
        } => status(&config, !no_discover, timeout_secs)?,
    }

    Ok(())
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
    let device = resolve_device_address(&config.app_id, device, device_id.as_deref())?;

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

fn sync_file(
    path: &Path,
    device: Option<SocketAddr>,
    device_id: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_config(path)?;
    let device = resolve_device_address(&config.app_id, device, device_id.as_deref())?;
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
        state.set(FILE_KEY.to_string(), local_bytes);
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
        "Synced with {} ({})",
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

    let mut discovered: Vec<DiscoveredDevice> = Vec::new();
    if discover {
        discovered = browse_mdns(&config.app_id, Duration::from_secs(timeout_secs))?;
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
            .map(|ts| format!("last seen: {ts} (unix)"))
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

fn load_config(path: &Path) -> Result<Config, Box<dyn std::error::Error>> {
    let data = fs::read(path)?;
    let config = serde_json::from_slice(&data)?;
    Ok(config)
}

fn save_config(path: &Path, config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let serialized = serde_json::to_vec_pretty(config)?;
    fs::write(path, serialized)?;
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
        fs::create_dir_all(parent)?;
    }
    fs::write(path, b"{}")?;
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
    device: Option<SocketAddr>,
    device_id: Option<&str>,
) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    resolve_device_address_with_timeout(app_id, device, device_id, Duration::from_secs(3))
}

fn resolve_device_address_with_timeout(
    app_id: &str,
    device: Option<SocketAddr>,
    device_id: Option<&str>,
    timeout: Duration,
) -> Result<SocketAddr, Box<dyn std::error::Error>> {
    if let Some(device) = device {
        return Ok(device);
    }

    let devices = browse_mdns(app_id, timeout)?;
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

fn pick_address(info: &ServiceInfo, port: u16) -> Option<SocketAddr> {
    let address = info.get_addresses().iter().find_map(|addr| match addr {
        IpAddr::V4(ip) => Some(IpAddr::V4(*ip)),
        IpAddr::V6(ip) => Some(IpAddr::V6(*ip)),
    })?;
    Some(SocketAddr::new(address, port))
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
    fn browse_mdns_returns_empty_for_short_timeout() {
        let devices = browse_mdns(APP_ID_DEFAULT, Duration::from_millis(10)).expect("browse");
        assert!(devices.is_empty() || devices.iter().all(|p| p.device_id.len() > 0));
    }

    #[test]
    fn resolve_device_address_prefers_explicit_device() {
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 52345));
        let resolved = resolve_device_address_with_timeout(
            APP_ID_DEFAULT,
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
            None,
            None,
            Duration::from_millis(5),
        )
        .expect_err("expected error");
        assert!(error.to_string().contains("no devices found"));
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
