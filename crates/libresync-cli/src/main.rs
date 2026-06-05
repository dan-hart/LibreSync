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
use std::time::{Duration, Instant, SystemTime};

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use chrono::{Local, TimeZone};
use clap::{Parser, Subcommand, ValueEnum};
use directories::ProjectDirs;
use libresync::{
    decrypt_entries, encrypt_entries, summarize_snapshot_diff, AppKey, BackupManager, DataAdapter,
    DataAdapterBackup, DeviceHandler, DeviceInfo, DeviceKeys, Engine, EngineConfig,
    FileLogicalAdapter, FileSnapshotStore, Identity, JsonFileAdapter, LogicalAdapterWrapper,
    RestoreOptions, RetentionPolicy, SnapshotStore, SqliteFileAdapter, State,
};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use rand::seq::IndexedRandom;
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, System};

const APP_ID_DEFAULT: &str = "com.codedbydan.libresync-cli";
const FILE_KEY: &str = "file";
const DEFAULT_LISTEN: &str = "0.0.0.0:52345";
const CONFIG_FILE_NAME: &str = "libresync.json";
const CONFIG_QUALIFIER: &str = "com";
const CONFIG_ORG: &str = "codedbydan";
const CONFIG_APP: &str = "libresync-cli";
const AUTO_APPROVE_INTERVAL_SECS: u64 = 5;
const AUTO_APPROVE_RETRY_SECS: u64 = 300;
const AUTO_APPROVE_DISCOVER_TIMEOUT_SECS: u64 = 2;

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
    about = "Local-only device-to-device sync CLI for LibreSync.",
    long_about = "A practical CLI for the LibreSync core. Use it to create configs, select logical/JSON/SQLite adapters,\ndiscover devices on LAN/private overlays, link devices, refresh/watch adapters, and manage encrypted backups and keys.",
    after_help = "Examples:\n  libresync init --app-id com.example.notes\n  libresync select --file ./data.json\n  libresync select --id settings --file ./settings.json\n  libresync select --id db --kind sqlite --page-delta 4096 --file ./app.db\n  libresync select --id records --kind logical-file --file ./records.json\n  libresync listen\n  libresync link\n  libresync refresh --all\n  libresync refresh --all-adapters\n  libresync watch --interval-secs 5\n  libresync device set-address --device-id amber-river-summit --address 192.168.1.10:52345\n  libresync backup configure --enable\n  libresync backup snapshot --note \"before import\"\n  libresync backup list\n  libresync backup restore --snapshot-id <id> --confirm --confirm-id <id>\n  libresync status\n\nOutput hints:\n  - Most commands print the config path in use and adapter IDs affected.\n  - Link/refresh output includes local and remote fingerprints for trust checks.\n  - Snapshot/export commands print the snapshot ID or output file path."
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
        long_about = "Creates a config file that stores the device ID, user ID, app ID, allowlisted devices, and internal state paths. This is required before discovery, linking, or refresh. Use --force to overwrite an existing config.",
        after_help = "Examples:\n  libresync init\n  libresync init --app-id com.example.notes\n  libresync init --config ./libresync.json\n\nOutput:\n  - prints the config path\n  - prints device ID, user ID, app ID, and fingerprint"
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
            long_help = "App bundle identifier used to scope discovery and trust. Devices only link and refresh when app IDs match."
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
            long_help = "Override the generated user ID. Use an adjective-noun link separated by a dash (e.g. calm-forest)."
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
        about = "Select a file-backed adapter to keep refreshed.",
        long_about = "Sets the file path for an adapter (json, sqlite, or logical-file). The file is created if it does not exist. SQLite adapters may enable page-delta encoding. Logical-file adapters store structured records as JSON.",
        after_help = "Examples:\n  libresync select --file ./data.json\n  libresync select --id settings --file ./settings.json\n  libresync select --id db --kind sqlite --page-delta 4096 --file ./app.db\n  libresync select --id records --kind logical-file --file ./records.json\n\nOutput:\n  - prints adapter ID, kind, and path\n  - updates the default adapter when the ID is \"file\""
    )]
    Select {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory).",
            long_help = "Config file that stores the selected adapter paths. Defaults to the OS config directory when omitted."
        )]
        config: Option<PathBuf>,
        #[arg(
            long,
            default_value = FILE_KEY,
            help = "Adapter ID to assign this file to."
        )]
        id: String,
        #[arg(
            long,
            value_enum,
            default_value = "json",
            help = "Adapter kind (json, sqlite, or logical-file)."
        )]
        kind: AdapterKindArg,
        #[arg(
            long,
            help = "SQLite page-delta size in bytes (enables delta encoding)."
        )]
        page_delta: Option<usize>,
        #[arg(long, help = "Logical namespace override (defaults to the app ID).")]
        namespace: Option<String>,
        #[arg(
            long,
            help = "Path to the file for this adapter.",
            long_help = "Path to the file that will be refreshed between devices. The file is created if missing."
        )]
        file: PathBuf,
    },
    #[command(
        about = "Discover devices on the LAN running the same app ID.",
        long_about = "List devices discovered for the current app ID. Discovery uses LAN mDNS plus private overlays (Tailscale/Headscale when available) and does not grant trust.",
        after_help = "Examples:\n  libresync discover\n  libresync discover --timeout-secs 6\n\nOutput:\n  - list of device IDs, user IDs, and addresses"
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
            help = "Duration in seconds to wait for discovery responses.",
            long_help = "Time to wait for discovery responses (LAN mDNS + private overlays). Increase this value on slower networks."
        )]
        timeout_secs: u64,
    },
    #[command(
        about = "Link with a device by address (requires device consent).",
        long_about = "Link establishes trust only. It does not refresh any data. The remote device must accept the linking request, and you must confirm locally unless --yes is set.",
        after_help = "Examples:\n  libresync link --device 192.168.1.10:52345\n  libresync link --device-id amber-river-summit\n  libresync link --yes\n\nOutput:\n  - local and remote fingerprints\n  - linking result and stored allowlist entry"
    )]
    Link {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory).",
            long_help = "Config file to store the linked device entry and trust state. Defaults to the OS config directory when omitted."
        )]
        config: Option<PathBuf>,
        #[arg(
            long,
            help = "Device address to connect to (e.g. 192.168.1.10:52345).",
            long_help = "Socket address for the device listener you want to link with. If omitted, LibreSync will try discovery (LAN + private overlays)."
        )]
        device: Option<SocketAddr>,
        #[arg(
            long,
            conflicts_with = "device",
            help = "Device ID to link with (uses discovery).",
            long_help = "Device ID to link with. LibreSync will discover devices (LAN + private overlays) and connect to the matching device ID."
        )]
        device_id: Option<String>,
        #[arg(
            long,
            help = "Auto-accept the local linking prompt.",
            long_help = "Skip the local confirmation prompt after the remote device accepts."
        )]
        yes: bool,
    },
    #[command(
        about = "Manage linked device metadata.",
        long_about = "Set or update stored addresses for linked devices."
    )]
    Device {
        #[command(subcommand)]
        command: DeviceCommands,
    },
    #[command(
        about = "Revoke linking with a device.",
        long_about = "Removes a linked device from the local allowlist. The device will need to link again before any future refresh.",
        after_help = "Examples:\n  libresync unlink --device-id amber-river-summit\n\nOutput:\n  - confirmation that the device was removed"
    )]
    Unlink {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory).",
            long_help = "Config file that includes the linked devices allowlist. Defaults to the OS config directory when omitted."
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
        long_about = "Start a device listener that accepts inbound connections and (by default) advertises via mDNS. The listener runs in the background by default; use --foreground to keep it attached to your terminal.",
        after_help = "Examples:\n  libresync listen\n  libresync listen --listen 0.0.0.0:52345 --foreground\n  libresync listen --no-discovery\n  libresync listen --auto-approve\n\nOutput:\n  - prints the listen address\n  - background mode prints PID and log path"
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
            help = "Auto-accept incoming linking requests.",
            long_help = "Automatically approve linking requests from devices with the same app ID."
        )]
        auto_accept: bool,
        #[arg(
            long,
            help = "Auto-approve linking with discovered devices on private LANs.",
            long_help = "Automatically discover and link devices running the same app ID on private or link-local networks. Implies auto-accept for inbound links."
        )]
        auto_approve: bool,
        #[arg(
            long,
            default_value_t = 15,
            help = "Auto-approve duration in minutes (only with --auto-approve)."
        )]
        auto_approve_minutes: u64,
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
        about = "Refresh selected adapters with a linked device.",
        long_about = "Refresh exchanges data only after trust is established. It requires prior linking. Adapter data is loaded into local state before refresh and written back after refresh.",
        after_help = "Examples:\n  libresync refresh --device 192.168.1.10:52345\n  libresync refresh --device-id amber-river-summit\n  libresync refresh --all\n  libresync refresh --all --no-discover\n  libresync refresh --all-adapters\n\nOutput:\n  - per-device refresh summary\n  - adapter IDs updated and error hints"
    )]
    Refresh {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory).",
            long_help = "Config file that includes selected adapters and device allowlist. Defaults to the OS config directory when omitted."
        )]
        config: Option<PathBuf>,
        #[arg(
            long,
            help = "Adapter ID to refresh (defaults to the selected adapter)."
        )]
        adapter_id: Option<String>,
        #[arg(long, help = "Refresh all selected adapters.")]
        all_adapters: bool,
        #[arg(
            long,
            conflicts_with_all = ["device", "device_id"],
            help = "Refresh all linked devices (uses discovery by default).",
            long_help = "Refresh all linked devices instead of a single device. Uses LAN discovery by default; add --no-discover to only use stored addresses."
        )]
        all: bool,
        #[arg(
            long,
            requires = "all",
            help = "Skip LAN discovery when using --all.",
            long_help = "Skip LAN discovery for refresh-all and only use stored addresses from the config."
        )]
        no_discover: bool,
        #[arg(
            long,
            help = "Device address to connect to (e.g. 192.168.1.10:52345).",
            long_help = "Socket address for the device listener you want to refresh with. If omitted, LibreSync will try discovery (LAN + private overlays)."
        )]
        device: Option<SocketAddr>,
        #[arg(
            long,
            conflicts_with = "device",
            help = "Device ID to refresh with (uses discovery).",
            long_help = "Device ID to refresh with. LibreSync will discover devices (LAN + private overlays) and connect to the matching device ID."
        )]
        device_id: Option<String>,
    },
    #[command(
        about = "Watch selected adapters and refresh all linked devices.",
        long_about = "Watch selected adapter files for local changes and refresh with all linked devices. Refresh pulls and pushes data, providing best-effort bidirectional updates.",
        after_help = "Examples:\n  libresync watch\n  libresync watch --interval-secs 5\n  libresync watch --all-adapters --no-discover\n\nOutput:\n  - watch start banner with interval and debounce\n  - refresh summaries per cycle"
    )]
    Watch {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory).",
            long_help = "Config file that includes selected adapters and device allowlist. Defaults to the OS config directory when omitted."
        )]
        config: Option<PathBuf>,
        #[arg(long, help = "Adapter ID to watch (defaults to the selected adapter).")]
        adapter_id: Option<String>,
        #[arg(long, help = "Watch all selected adapters.")]
        all_adapters: bool,
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
            long_help = "Interval in seconds to refresh with linked devices even if no local change is detected."
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
            help = "Disable LAN discovery when resolving linked devices.",
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
        long_about = "Stop the background listener spawned by `libresync listen`. This reads the PID from the config directory and terminates the process.",
        after_help = "Examples:\n  libresync stop\n\nOutput:\n  - confirmation that the listener was stopped"
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
        long_about = "Create, list, and restore encrypted snapshots. Backups are opt-in per app, and restores require an explicit config flag plus --confirm.",
        after_help = "Examples:\n  libresync backup configure --enable\n  libresync backup snapshot --note \"before import\"\n  libresync backup list\n  libresync backup preview --snapshot-id <id>\n  libresync backup restore --snapshot-id <id> --confirm --confirm-id <id>\n\nOutput hints:\n  - snapshot commands print snapshot IDs\n  - restore/preview commands print a diff summary"
    )]
    Backup {
        #[command(subcommand)]
        command: BackupCommands,
    },
    #[command(
        about = "Show device status, linked devices, and recent discovery info.",
        long_about = "Show local identity, selected file, backup settings, listener status, linked devices, and (by default) devices discovered on the LAN. Discovered devices are treated as connected now. Use --no-discover to skip LAN discovery.",
        after_help = "Examples:\n  libresync status\n  libresync status --no-discover\n\nOutput:\n  - identity, adapters, backups, listener status\n  - linked devices with last seen address"
    )]
    Status {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory).",
            long_help = "Config file that includes identity, selected file, and linked devices. Defaults to the OS config directory when omitted."
        )]
        config: Option<PathBuf>,
        #[arg(
            long,
            help = "Disable LAN discovery for this status check.",
            long_help = "Skip mDNS discovery and only show stored linked device information."
        )]
        no_discover: bool,
        #[arg(
            long,
            default_value_t = 3,
            help = "Duration in seconds to wait for discovery responses.",
            long_help = "Time to wait for discovery responses when status discovery is enabled."
        )]
        timeout_secs: u64,
    },
    #[command(
        about = "Run diagnostics for the current config.",
        long_about = "Check config, keys, state file, selected file, and backup settings. Useful for troubleshooting before linking or refresh.",
        after_help = "Examples:\n  libresync diagnose\n  libresync diagnose --config ./libresync.json\n\nOutput:\n  - checklist with pass/fail hints"
    )]
    Diagnose {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory)."
        )]
        config: Option<PathBuf>,
    },
    #[command(
        about = "Manage app-level encryption keys.",
        long_about = "Rotate, export, and import app-level encryption keys. App keys protect sync payloads, state files, and backups."
    )]
    Key {
        #[command(subcommand)]
        command: KeyCommands,
    },
}

#[derive(Subcommand)]
enum KeyCommands {
    #[command(
        about = "Rotate the app-level key.",
        long_about = "Re-encrypts state and backups with a new app key. Clears linked devices by default, requiring re-linking.",
        after_help = "Examples:\n  libresync key rotate --confirm\n  libresync key rotate --confirm --keep-allowlist\n\nOutput:\n  - reports re-encryption status and allowlist behavior"
    )]
    Rotate {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory)."
        )]
        config: Option<PathBuf>,
        #[arg(long, help = "Keep the existing allowlist (skip forced re-linking).")]
        keep_allowlist: bool,
        #[arg(long, help = "Confirm key rotation.")]
        confirm: bool,
    },
    #[command(
        about = "Export the app-level key.",
        long_about = "Writes the app-level encryption key to a file so it can be imported on another device.",
        after_help = "Examples:\n  libresync key export-app --output ./app.key\n\nOutput:\n  - path of the key file written"
    )]
    ExportApp {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory)."
        )]
        config: Option<PathBuf>,
        #[arg(long, help = "Output path for the key file.")]
        output: PathBuf,
    },
    #[command(
        about = "Import an app-level key.",
        long_about = "Replaces the app-level key and re-encrypts state/backups with the provided key.",
        after_help = "Examples:\n  libresync key import-app --input ./app.key --confirm\n\nOutput:\n  - reports re-encryption status and allowlist behavior"
    )]
    ImportApp {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory)."
        )]
        config: Option<PathBuf>,
        #[arg(long, help = "Path to the key file to import.")]
        input: PathBuf,
        #[arg(long, help = "Clear the allowlist to force re-linking.")]
        clear_allowlist: bool,
        #[arg(long, help = "Confirm key import.")]
        confirm: bool,
    },
    #[command(
        about = "Export the device keys.",
        long_about = "Writes the device TLS keys and fingerprint to a file.",
        after_help = "Examples:\n  libresync key export-device --output ./device.keys\n\nOutput:\n  - path of the device key file written"
    )]
    ExportDevice {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory)."
        )]
        config: Option<PathBuf>,
        #[arg(long, help = "Output path for the device key file.")]
        output: PathBuf,
    },
    #[command(
        about = "Import device keys.",
        long_about = "Replaces the device TLS keys with the provided file.",
        after_help = "Examples:\n  libresync key import-device --input ./device.keys --confirm\n\nOutput:\n  - reports the new fingerprint and allowlist behavior"
    )]
    ImportDevice {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory)."
        )]
        config: Option<PathBuf>,
        #[arg(long, help = "Path to the device key file to import.")]
        input: PathBuf,
        #[arg(long, help = "Clear the allowlist to force re-linking.")]
        clear_allowlist: bool,
        #[arg(long, help = "Confirm key import.")]
        confirm: bool,
    },
    #[command(
        about = "Rotate the device identity keys.",
        long_about = "Generates new device TLS keys and fingerprint. Remote devices must re-link to trust the new fingerprint.",
        after_help = "Examples:\n  libresync key rotate-device --confirm\n  libresync key rotate-device --confirm --clear-allowlist\n\nOutput:\n  - prints the new fingerprint and allowlist behavior"
    )]
    RotateDevice {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory)."
        )]
        config: Option<PathBuf>,
        #[arg(long, help = "Clear the local allowlist to force re-linking.")]
        clear_allowlist: bool,
        #[arg(long, help = "Confirm key rotation.")]
        confirm: bool,
    },
}

#[derive(Subcommand)]
enum DeviceCommands {
    #[command(
        about = "Store a manual address for a linked device.",
        long_about = "Overrides the stored address for a linked device to enable manual refresh when discovery fails.",
        after_help = "Examples:\n  libresync device set-address --device-id amber-river-summit --address 192.168.1.10:52345\n\nOutput:\n  - confirms the stored address and updates last seen"
    )]
    SetAddress {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory)."
        )]
        config: Option<PathBuf>,
        #[arg(long, help = "Linked device ID to update.")]
        device_id: String,
        #[arg(long, help = "Socket address to store for the device.")]
        address: SocketAddr,
    },
    #[command(
        about = "Toggle auto-approve linking for this device.",
        long_about = "When enabled, the listener will automatically link devices on the LAN running the same app ID. Auto-approve is opt-in and limited to private/link-local addresses.",
        after_help = "Examples:\n  libresync device auto-approve --enable\n  libresync device auto-approve --disable\n  libresync device auto-approve\n\nOutput:\n  - prints whether auto-approve is enabled"
    )]
    AutoApprove {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory)."
        )]
        config: Option<PathBuf>,
        #[arg(long, help = "Enable auto-approve linking.")]
        enable: bool,
        #[arg(long, help = "Disable auto-approve linking.")]
        disable: bool,
        #[arg(
            long,
            default_value_t = 15,
            help = "Minutes to keep auto-approve enabled (ignored with --persist)."
        )]
        minutes: u64,
        #[arg(long, help = "Keep auto-approve enabled until manually disabled.")]
        persist: bool,
    },
    #[command(
        about = "Set or clear the pairing secret used for auto-approve linking.",
        long_about = "When set, linking requests must include the pairing secret to be accepted. Use this to harden auto-approve linking on LAN.",
        after_help = "Examples:\n  libresync device pairing-secret --set \"shared-secret\"\n  libresync device pairing-secret --clear\n\nOutput:\n  - prints whether the pairing secret is set"
    )]
    PairingSecret {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory)."
        )]
        config: Option<PathBuf>,
        #[arg(long, help = "Set the pairing secret value.")]
        set: Option<String>,
        #[arg(long, help = "Clear the pairing secret.")]
        clear: bool,
    },
}

#[derive(Subcommand)]
enum BackupCommands {
    #[command(
        about = "Configure backup behavior for this app.",
        long_about = "Enable backups and optionally allow restores. This is a required opt-in per app.",
        after_help = "Examples:\n  libresync backup configure --enable\n  libresync backup configure --enable --allow-restore\n\nOutput:\n  - prints backup status and storage path"
    )]
    Configure {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory)."
        )]
        config: Option<PathBuf>,
        #[arg(long, help = "Enable encrypted backups for this app.")]
        enable: bool,
        #[arg(
            long,
            help = "Allow restores for this app (requires explicit confirmation at restore time)."
        )]
        allow_restore: bool,
        #[arg(long, help = "Optional backup directory override.")]
        dir: Option<PathBuf>,
    },
    #[command(
        about = "Create an encrypted snapshot.",
        long_about = "Create a new encrypted snapshot of the selected data for backups.",
        after_help = "Examples:\n  libresync backup snapshot\n  libresync backup snapshot --note \"before import\"\n\nOutput:\n  - snapshot ID and storage path"
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
        #[arg(long, help = "Optional note to attach to the snapshot.")]
        note: Option<String>,
    },
    #[command(
        about = "List encrypted snapshots.",
        long_about = "List encrypted snapshots stored for the selected adapter.",
        after_help = "Examples:\n  libresync backup list\n\nOutput:\n  - table of snapshot IDs, timestamps, and sizes"
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
        long_about = "Shows a diff summary between a snapshot and the current state without restoring.",
        after_help = "Examples:\n  libresync backup preview --snapshot-id <id>\n\nOutput:\n  - diff summary (added/updated/removed)"
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
        #[arg(long, help = "Snapshot ID to preview.")]
        snapshot_id: String,
    },
    #[command(
        about = "Restore an encrypted snapshot.",
        long_about = "Restore a snapshot into the selected data. Requires backup allow-restore config, --confirm, and --confirm-id.",
        after_help = "Examples:\n  libresync backup restore --snapshot-id <id> --confirm --confirm-id <id>\n\nOutput:\n  - restore result and updated adapter path"
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
        #[arg(long, help = "Snapshot ID to restore.")]
        snapshot_id: String,
        #[arg(long, help = "Confirm the restore action.")]
        confirm: bool,
        #[arg(long, help = "Confirm snapshot ID (must match the snapshot_id).")]
        confirm_id: Option<String>,
    },
    #[command(
        about = "Prune old encrypted snapshots.",
        long_about = "Delete old snapshots using retention rules. Provide at least one of --keep or --max-age-days.",
        after_help = "Examples:\n  libresync backup prune --keep 10\n  libresync backup prune --max-age-days 30\n  libresync backup prune --keep 5 --max-age-days 14 --dry-run\n\nOutput:\n  - prune plan and items removed (or to be removed)"
    )]
    Prune {
        #[arg(
            long,
            help = "Path to the config JSON file (defaults to the OS config directory)."
        )]
        config: Option<PathBuf>,
        #[arg(
            long,
            default_value = FILE_KEY,
            help = "Adapter ID to prune (default: file)."
        )]
        adapter_id: String,
        #[arg(long, help = "Keep at most this many snapshots.")]
        keep: Option<usize>,
        #[arg(long, help = "Delete snapshots older than this many days.")]
        max_age_days: Option<u64>,
        #[arg(long, help = "Show what would be deleted without removing snapshots.")]
        dry_run: bool,
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
    data_paths: BTreeMap<String, PathBuf>,
    #[serde(default)]
    default_adapter: Option<String>,
    #[serde(default)]
    adapters: BTreeMap<String, AdapterConfig>,
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
    auto_approve: bool,
    #[serde(default)]
    auto_approve_until: Option<u64>,
    #[serde(default)]
    pairing_secret: Option<String>,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum AdapterKindConfig {
    Json,
    Sqlite,
    LogicalFile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdapterConfig {
    kind: AdapterKindConfig,
    path: PathBuf,
    #[serde(default)]
    page_delta: Option<usize>,
    #[serde(default)]
    namespace: Option<String>,
}

#[derive(Clone, Debug, ValueEnum)]
enum AdapterKindArg {
    Json,
    Sqlite,
    LogicalFile,
}

impl From<AdapterKindArg> for AdapterKindConfig {
    fn from(value: AdapterKindArg) -> Self {
        match value {
            AdapterKindArg::Json => AdapterKindConfig::Json,
            AdapterKindArg::Sqlite => AdapterKindConfig::Sqlite,
            AdapterKindArg::LogicalFile => AdapterKindConfig::LogicalFile,
        }
    }
}

impl DeviceKeysRecord {
    fn from_keys(keys: &DeviceKeys) -> Self {
        Self {
            cert_der: BASE64.encode(keys.cert_der()),
            key_der: BASE64.encode(keys.key_der()),
            fingerprint: keys.fingerprint().to_string(),
        }
    }

    fn to_keys(&self) -> Result<DeviceKeys, Box<dyn std::error::Error>> {
        let cert_der = BASE64
            .decode(self.cert_der.as_bytes())
            .map_err(|error| format!("failed to decode cert: {error}"))?;
        let key_der = BASE64
            .decode(self.key_der.as_bytes())
            .map_err(|error| format!("failed to decode key: {error}"))?;
        let keys = DeviceKeys::from_der(cert_der, key_der)?;
        Ok(keys)
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

    fn adapter_config(&self, adapter_id: &str) -> Option<AdapterConfig> {
        if let Some(config) = self.adapters.get(adapter_id) {
            return Some(config.clone());
        }
        if let Some(path) = self.data_paths.get(adapter_id) {
            return Some(AdapterConfig {
                kind: AdapterKindConfig::Json,
                path: path.clone(),
                page_delta: None,
                namespace: None,
            });
        }
        if adapter_id == FILE_KEY {
            return self.data_path.clone().map(|path| AdapterConfig {
                kind: AdapterKindConfig::Json,
                path,
                page_delta: None,
                namespace: None,
            });
        }
        None
    }

    fn adapter_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.adapters.keys().cloned().collect();
        if ids.is_empty() {
            ids = self.data_paths.keys().cloned().collect();
            if ids.is_empty() && self.data_path.is_some() {
                ids.push(FILE_KEY.to_string());
            }
        }
        ids.sort();
        ids.dedup();
        ids
    }

    fn default_adapter_id(&self) -> Option<String> {
        if let Some(id) = self.default_adapter.as_ref() {
            if self.adapters.contains_key(id) {
                return Some(id.clone());
            }
        }
        if self.adapters.contains_key(FILE_KEY)
            || self.data_paths.contains_key(FILE_KEY)
            || self.data_path.is_some()
        {
            return Some(FILE_KEY.to_string());
        }
        self.adapters
            .keys()
            .next()
            .cloned()
            .or_else(|| self.data_paths.keys().next().cloned())
    }

    fn set_adapter_config(
        &mut self,
        adapter_id: &str,
        kind: AdapterKindConfig,
        path: PathBuf,
        page_delta: Option<usize>,
        namespace: Option<String>,
    ) {
        self.adapters.insert(
            adapter_id.to_string(),
            AdapterConfig {
                kind,
                path: path.clone(),
                page_delta,
                namespace,
            },
        );
        self.default_adapter = Some(adapter_id.to_string());
        if kind == AdapterKindConfig::Json {
            self.data_paths.insert(adapter_id.to_string(), path.clone());
            if adapter_id == FILE_KEY {
                self.data_path = Some(path);
            }
        }
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

    fn is_linked(&self, device_id: &str) -> bool {
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

    fn auto_approve_state(&mut self) -> (bool, bool) {
        if !self.auto_approve {
            return (false, false);
        }
        if let Some(until) = self.auto_approve_until {
            if now_unix_secs() > until {
                self.auto_approve = false;
                self.auto_approve_until = None;
                return (false, true);
            }
        }
        (true, false)
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

    fn is_linked(&self, identity: &Identity) -> bool {
        let config = self.config.lock().expect("config lock");
        config.is_linked(&identity.device_id)
    }

    fn approve_link(&self, identity: &Identity) -> libresync::Result<bool> {
        self.approve_link_with_fingerprint(identity, "")
    }

    fn device_keys(&self) -> libresync::Result<DeviceKeys> {
        let config = self.config.lock().expect("config lock");
        config
            .device_keys()
            .map_err(|error| libresync::Error::Protocol(error.to_string()))
    }

    fn set_device_keys(&self, device_keys: &DeviceKeys) -> libresync::Result<()> {
        let mut config = self.config.lock().expect("config lock");
        config.device_keys = Some(DeviceKeysRecord::from_keys(device_keys));
        save_config(&self.config_path, &config)
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

    fn is_linked_with_fingerprint(&self, identity: &Identity, fingerprint: &str) -> bool {
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
        identity: &Identity,
        fingerprint: &str,
    ) -> libresync::Result<bool> {
        if identity.app_id != self.app_id {
            return Ok(false);
        }
        let (auto_accept, expired) = {
            let mut config = self.config.lock().expect("config lock");
            let (auto_approve, expired) = config.auto_approve_state();
            (self.auto_accept || auto_approve, expired)
        };
        if expired {
            if let Ok(config) = self.config.lock() {
                let _ = save_config(&self.config_path, &config);
            }
        }
        let accepted = if auto_accept {
            true
        } else {
            prompt_yes_no(&format!(
                "Link with {} ({})? [y/N]: ",
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

    fn pairing_secret(&self) -> Option<String> {
        self.config
            .lock()
            .ok()
            .and_then(|config| config.pairing_secret.clone())
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

    fn is_linked(&self, identity: &Identity) -> bool {
        self.allowed.contains_key(&identity.device_id)
    }

    fn approve_link(&self, _identity: &Identity) -> libresync::Result<bool> {
        Ok(false)
    }

    fn device_keys(&self) -> libresync::Result<DeviceKeys> {
        Ok(self.keys.clone())
    }

    fn app_key(&self) -> libresync::Result<AppKey> {
        Ok(self.app_key.clone())
    }

    fn is_linked_with_fingerprint(&self, identity: &Identity, fingerprint: &str) -> bool {
        self.allowed
            .get(&identity.device_id)
            .map(|stored| stored == fingerprint)
            .unwrap_or(false)
    }
}

fn engine_for_config(config: &Config) -> Engine {
    let device_keys = config.device_keys().expect("device keys should exist");
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
        Commands::Select {
            config,
            id,
            kind,
            page_delta,
            namespace,
            file,
        } => {
            let config = resolve_config_path(config);
            select_file(&config, &id, kind.into(), page_delta, namespace, &file)?;
        }
        Commands::Discover {
            config,
            timeout_secs,
        } => {
            let config = resolve_config_path(config);
            discover_devices(&config, timeout_secs)?;
        }
        Commands::Link {
            config,
            device,
            device_id,
            yes,
        } => {
            let config = resolve_config_path(config);
            link_device(&config, device, device_id, yes)?;
        }
        Commands::Device { command } => match command {
            DeviceCommands::SetAddress {
                config,
                device_id,
                address,
            } => {
                let config = resolve_config_path(config);
                set_device_address(&config, &device_id, address)?;
            }
            DeviceCommands::AutoApprove {
                config,
                enable,
                disable,
                minutes,
                persist,
            } => {
                let config = resolve_config_path(config);
                set_auto_approve(&config, enable, disable, minutes, persist)?;
            }
            DeviceCommands::PairingSecret { config, set, clear } => {
                let config = resolve_config_path(config);
                set_pairing_secret(&config, set, clear)?;
            }
        },
        Commands::Unlink {
            config,
            device_id,
            yes,
        } => {
            let config = resolve_config_path(config);
            unlink_device(&config, &device_id, yes)?;
        }
        Commands::Listen {
            config,
            listen,
            auto_accept,
            auto_approve,
            auto_approve_minutes,
            no_discovery,
            foreground,
            duration_secs,
        } => {
            let config = resolve_config_path(config);
            if foreground {
                listen_device(
                    &config,
                    listen,
                    auto_accept,
                    auto_approve,
                    auto_approve_minutes,
                    no_discovery,
                    duration_secs,
                )?;
            } else {
                spawn_background_listener(
                    &config,
                    listen,
                    auto_accept,
                    auto_approve,
                    auto_approve_minutes,
                    no_discovery,
                    duration_secs,
                )?;
            }
        }
        Commands::Refresh {
            config,
            adapter_id,
            all_adapters,
            device,
            device_id,
            all,
            no_discover,
        } => {
            let config = resolve_config_path(config);
            if all {
                refresh_all(&config, !no_discover, adapter_id.as_deref(), all_adapters)?;
            } else {
                refresh_file(
                    &config,
                    device,
                    device_id,
                    adapter_id.as_deref(),
                    all_adapters,
                )?;
            }
        }
        Commands::Watch {
            config,
            adapter_id,
            all_adapters,
            listen,
            interval_secs,
            debounce_ms,
            no_discover,
            no_listen,
        } => {
            let config = resolve_config_path(config);
            watch_file(
                &config,
                adapter_id.as_deref(),
                all_adapters,
                listen,
                interval_secs,
                debounce_ms,
                no_discover,
                no_listen,
            )?;
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
            BackupCommands::Prune {
                config,
                adapter_id,
                keep,
                max_age_days,
                dry_run,
            } => {
                let config = resolve_config_path(config);
                prune_backup_snapshots(&config, &adapter_id, keep, max_age_days, dry_run)?;
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
        Commands::Diagnose { config } => {
            let config = resolve_config_path(config);
            diagnose(&config)?;
        }
        Commands::Key { command } => match command {
            KeyCommands::Rotate {
                config,
                keep_allowlist,
                confirm,
            } => {
                let config = resolve_config_path(config);
                rotate_app_key(&config, keep_allowlist, confirm)?;
            }
            KeyCommands::RotateDevice {
                config,
                clear_allowlist,
                confirm,
            } => {
                let config = resolve_config_path(config);
                rotate_device_keys(&config, clear_allowlist, confirm)?;
            }
            KeyCommands::ExportApp { config, output } => {
                let config = resolve_config_path(config);
                export_app_key(&config, &output)?;
            }
            KeyCommands::ImportApp {
                config,
                input,
                clear_allowlist,
                confirm,
            } => {
                let config = resolve_config_path(config);
                import_app_key(&config, &input, clear_allowlist, confirm)?;
            }
            KeyCommands::ExportDevice { config, output } => {
                let config = resolve_config_path(config);
                export_device_keys(&config, &output)?;
            }
            KeyCommands::ImportDevice {
                config,
                input,
                clear_allowlist,
                confirm,
            } => {
                let config = resolve_config_path(config);
                import_device_keys(&config, &input, clear_allowlist, confirm)?;
            }
        },
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
    let device_keys =
        DeviceKeys::generate(&identity).map_err(|error| io::Error::other(error.to_string()))?;
    let app_key = AppKey::generate().map_err(|error| io::Error::other(error.to_string()))?;

    let config = Config {
        device_id,
        app_id: app_id.to_string(),
        user_id,
        state_path,
        data_path: None,
        data_paths: BTreeMap::new(),
        default_adapter: None,
        adapters: BTreeMap::new(),
        device_keys: Some(DeviceKeysRecord::from_keys(&device_keys)),
        app_key: Some(BASE64.encode(app_key.as_bytes())),
        backup_enabled: false,
        backup_allow_restore: false,
        backup_dir: None,
        auto_approve: false,
        auto_approve_until: None,
        pairing_secret: None,
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

fn select_file(
    path: &Path,
    adapter_id: &str,
    kind: AdapterKindConfig,
    page_delta: Option<usize>,
    namespace: Option<String>,
    file: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_config(path)?;
    let file = file.to_path_buf();
    if kind != AdapterKindConfig::Sqlite && page_delta.is_some() {
        return Err("page-delta is only supported for sqlite adapters".into());
    }
    match kind {
        AdapterKindConfig::Json => ensure_json_file(&file)?,
        AdapterKindConfig::Sqlite => ensure_file(&file)?,
        AdapterKindConfig::LogicalFile => ensure_logical_file(&file)?,
    }
    let namespace = match kind {
        AdapterKindConfig::LogicalFile => Some(namespace.unwrap_or_else(|| config.app_id.clone())),
        _ => None,
    };
    config.set_adapter_config(adapter_id, kind, file.clone(), page_delta, namespace);
    save_config(path, &config)?;
    println!(
        "Selected adapter {adapter_id} ({:?}): {}",
        kind,
        file.display()
    );
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

fn list_backup_snapshots(path: &Path, adapter_id: &str) -> Result<(), Box<dyn std::error::Error>> {
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
        return Err("restores are disabled; run libresync backup configure --allow-restore".into());
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

    manager.restore_snapshot(
        &backup_adapter,
        &mut state,
        snapshot_id,
        RestoreOptions::confirmed(),
    )?;
    adapter.apply_from_state(&state)?;
    state.save_encrypted(&config.app_key()?, &config.state_path)?;

    println!("Restored snapshot {}", snapshot_id);
    Ok(())
}

fn prune_backup_snapshots(
    path: &Path,
    adapter_id: &str,
    keep: Option<usize>,
    max_age_days: Option<u64>,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_config(path)?;
    let backup_dir = ensure_backup_dir(path, &mut config)?;
    if !config.backup_enabled {
        return Err("backups are not enabled; run libresync backup configure --enable".into());
    }

    let policy = RetentionPolicy {
        max_snapshots: keep,
        max_age_secs: max_age_days.map(|days| days.saturating_mul(24 * 60 * 60)),
    };
    if policy.is_empty() {
        return Err("prune requires --keep and/or --max-age-days".into());
    }

    let app_key = config.app_key()?;
    let store = FileSnapshotStore::new(&backup_dir)?;
    let manager = BackupManager::new(app_key, Arc::new(store));

    let plan = manager.plan_prune(adapter_id, policy.clone())?;
    if plan.to_delete.is_empty() {
        println!("No snapshots to prune for adapter {}", adapter_id);
        return Ok(());
    }

    if dry_run {
        println!(
            "Dry run: {} snapshot(s) would be deleted for adapter {}.",
            plan.to_delete.len(),
            adapter_id
        );
        for snapshot_id in plan.to_delete {
            println!("  {snapshot_id}");
        }
        return Ok(());
    }

    let summary = manager.prune_snapshots(adapter_id, policy)?;
    println!(
        "Deleted {} snapshot(s); {} remaining for adapter {}.",
        summary.deleted.len(),
        summary.remaining,
        adapter_id
    );
    for snapshot_id in summary.deleted {
        println!("  removed {snapshot_id}");
    }
    Ok(())
}

fn rotate_app_key(
    path: &Path,
    keep_allowlist: bool,
    confirm: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !confirm {
        return Err("key rotation requires --confirm".into());
    }

    let mut config = load_config(path)?;
    let old_key = config.app_key()?;
    let new_key = AppKey::generate()?;

    let state = match State::load_maybe_encrypted(&old_key, &config.state_path) {
        Ok(state) => state,
        Err(_) => State::new(config.device_id.clone()),
    };
    state.save_encrypted(&new_key, &config.state_path)?;

    let mut rotated = 0usize;
    if config.backup_enabled {
        let backup_dir = config.backup_dir(path);
        let store = FileSnapshotStore::new(&backup_dir)?;
        let manager = BackupManager::new(old_key.clone(), Arc::new(store.clone()));
        let snapshots = manager.list_snapshots(FILE_KEY)?;
        for snapshot in snapshots {
            let stored = store.load_snapshot(FILE_KEY, &snapshot.id)?;
            let entries = decrypt_entries(&old_key, stored.entries)?;
            let encrypted = encrypt_entries(&new_key, entries)?;
            let updated = libresync::Snapshot {
                metadata: stored.metadata,
                entries: encrypted,
            };
            store.save_snapshot(&updated)?;
            rotated += 1;
        }
    }

    config.set_app_key(&new_key);
    if !keep_allowlist {
        config.devices.clear();
    }
    save_config(path, &config)?;

    println!("Rotated app key.");
    if rotated > 0 {
        println!("Re-encrypted {rotated} snapshot(s).");
    }
    if keep_allowlist {
        println!("Allowlist kept; re-linking not required.");
    } else {
        println!("Allowlist cleared; re-link devices before refresh.");
    }

    Ok(())
}

fn rotate_device_keys(
    path: &Path,
    clear_allowlist: bool,
    confirm: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !confirm {
        return Err("device key rotation requires --confirm".into());
    }

    let mut config = load_config(path)?;
    let identity = config.identity();
    let new_keys = DeviceKeys::generate(&identity)?;
    config.device_keys = Some(DeviceKeysRecord::from_keys(&new_keys));
    if clear_allowlist {
        config.devices.clear();
    }
    save_config(path, &config)?;

    println!("Rotated device keys.");
    println!("New fingerprint: {}", new_keys.fingerprint());
    if clear_allowlist {
        println!("Allowlist cleared; re-link devices before refresh.");
    } else {
        println!("Remote devices must re-link to trust this fingerprint.");
    }
    println!("Restart listeners to apply the new keys.");
    Ok(())
}

fn export_app_key(path: &Path, output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config(path)?;
    let key = config.app_key()?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(output, BASE64.encode(key.as_bytes()))?;
    println!("App key exported to {}", output.display());
    Ok(())
}

fn import_app_key(
    path: &Path,
    input: &Path,
    clear_allowlist: bool,
    confirm: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !confirm {
        return Err("app key import requires --confirm".into());
    }

    let mut config = load_config(path)?;
    let old_key = config.app_key()?;
    let raw = fs::read_to_string(input)?;
    let bytes = BASE64
        .decode(raw.trim().as_bytes())
        .map_err(|error| format!("failed to decode app key: {error}"))?;
    let new_key = AppKey::from_slice(&bytes)?;

    let state = match State::load_maybe_encrypted(&old_key, &config.state_path) {
        Ok(state) => state,
        Err(_) => State::new(config.device_id.clone()),
    };
    state.save_encrypted(&new_key, &config.state_path)?;

    let mut rotated = 0usize;
    if config.backup_enabled {
        let backup_dir = config.backup_dir(path);
        let store = FileSnapshotStore::new(&backup_dir)?;
        let manager = BackupManager::new(old_key.clone(), Arc::new(store.clone()));
        let snapshots = manager.list_snapshots(FILE_KEY)?;
        for snapshot in snapshots {
            let stored = store.load_snapshot(FILE_KEY, &snapshot.id)?;
            let entries = decrypt_entries(&old_key, stored.entries)?;
            let encrypted = encrypt_entries(&new_key, entries)?;
            let updated = libresync::Snapshot {
                metadata: stored.metadata,
                entries: encrypted,
            };
            store.save_snapshot(&updated)?;
            rotated += 1;
        }
    }

    config.set_app_key(&new_key);
    if clear_allowlist {
        config.devices.clear();
    }
    save_config(path, &config)?;

    println!("Imported app key.");
    if rotated > 0 {
        println!("Re-encrypted {rotated} snapshot(s).");
    }
    if clear_allowlist {
        println!("Allowlist cleared; re-link devices before refresh.");
    }
    Ok(())
}

fn export_device_keys(path: &Path, output: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config(path)?;
    let record = config
        .device_keys
        .as_ref()
        .ok_or("device keys missing; re-run libresync init")?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_vec_pretty(record)?;
    fs::write(output, data)?;
    println!("Device keys exported to {}", output.display());
    Ok(())
}

fn import_device_keys(
    path: &Path,
    input: &Path,
    clear_allowlist: bool,
    confirm: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !confirm {
        return Err("device key import requires --confirm".into());
    }

    let mut config = load_config(path)?;
    let data = fs::read(input)?;
    let record: DeviceKeysRecord = serde_json::from_slice(&data)?;
    let keys = record.to_keys()?;
    config.device_keys = Some(DeviceKeysRecord::from_keys(&keys));
    if clear_allowlist {
        config.devices.clear();
    }
    save_config(path, &config)?;

    println!("Imported device keys.");
    println!("Fingerprint: {}", keys.fingerprint());
    if clear_allowlist {
        println!("Allowlist cleared; re-link devices before refresh.");
    }
    println!("Restart listeners to apply the new keys.");
    Ok(())
}

fn backup_adapter_for_config(
    config: &Config,
    adapter_id: &str,
) -> Result<(Arc<dyn DataAdapter>, DataAdapterBackup), Box<dyn std::error::Error>> {
    let adapter = build_adapter(adapter_id, config)?;
    let backup_adapter = DataAdapterBackup::new(adapter.clone());
    Ok((adapter, backup_adapter))
}

fn ensure_backup_dir(
    path: &Path,
    config: &mut Config,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if config.backup_dir.is_none() {
        config.set_backup_dir(default_backup_dir(path));
        save_config(path, config)?;
    }
    Ok(config.backup_dir(path))
}

fn print_snapshot_summary(
    adapter_id: &str,
    snapshot_id: &str,
    summary: &libresync::SnapshotDiffSummary,
) {
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

fn link_device(
    path: &Path,
    device: Option<SocketAddr>,
    device_id: Option<String>,
    auto_accept: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_config(path)?;
    let device = resolve_device_address(&config, device, device_id.as_deref())?;
    let engine = engine_for_config(&config);
    let remote_device = engine.request_link(device)?;
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
            "Confirm linking with {} ({})? [y/N]: ",
            remote_identity.device_id, remote_identity.user_id
        ))
        .unwrap_or(false)
    };

    if !local_accept {
        return Err("linking aborted locally".into());
    }

    config.upsert_device(
        &remote_identity,
        Some(device),
        remote_device.fingerprint.clone(),
    );
    save_config(path, &config)?;

    println!(
        "Linked with {} ({})",
        remote_identity.device_id, remote_identity.user_id
    );

    Ok(())
}

fn unlink_device(
    path: &Path,
    device_id: &str,
    auto_accept: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_config(path)?;
    if !config.devices.contains_key(device_id) {
        return Err(format!("device not linked: {device_id}").into());
    }

    let confirmed = if auto_accept {
        true
    } else {
        prompt_yes_no(&format!("Revoke linking with {device_id}? [y/N]: ")).unwrap_or(false)
    };

    if !confirmed {
        return Err("unlink aborted locally".into());
    }

    config.devices.remove(device_id);
    save_config(path, &config)?;

    println!("Unlinked {device_id}");
    Ok(())
}

fn set_device_address(
    path: &Path,
    device_id: &str,
    address: SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_config(path)?;
    let record = config
        .devices
        .get_mut(device_id)
        .ok_or_else(|| format!("device not linked: {device_id}"))?;
    record.last_seen_addr = Some(address.to_string());
    record.last_seen_unix_secs = Some(now_unix_secs());
    save_config(path, &config)?;
    println!("Stored address for {device_id}: {address}");
    Ok(())
}

fn set_auto_approve(
    path: &Path,
    enable: bool,
    disable: bool,
    minutes: u64,
    persist: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if enable && disable {
        return Err("choose either --enable or --disable".into());
    }

    let mut config = load_config(path)?;
    if enable {
        config.auto_approve = true;
        if persist {
            config.auto_approve_until = None;
        } else {
            let duration = minutes.max(1);
            config.auto_approve_until = Some(now_unix_secs() + duration * 60);
        }
        save_config(path, &config)?;
        if persist {
            println!("Auto-approve linking: enabled (no expiry, private/link-local only).");
        } else {
            println!(
                "Auto-approve linking: enabled for {minutes} minutes (private/link-local only)."
            );
        }
        return Ok(());
    }
    if disable {
        config.auto_approve = false;
        config.auto_approve_until = None;
        save_config(path, &config)?;
        println!("Auto-approve linking: disabled.");
        return Ok(());
    }

    let (active, expired) = config.auto_approve_state();
    if expired {
        save_config(path, &config)?;
    }
    let status = if active { "enabled" } else { "disabled" };
    if active {
        if let Some(until) = config.auto_approve_until {
            println!(
                "Auto-approve linking: {status} ({})",
                format_time_label("expires", until)
            );
        } else {
            println!("Auto-approve linking: {status} (no expiry).");
        }
    } else {
        println!("Auto-approve linking: {status}.");
    }
    Ok(())
}

fn set_pairing_secret(
    path: &Path,
    set: Option<String>,
    clear: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if clear && set.is_some() {
        return Err("choose either --set or --clear".into());
    }

    let mut config = load_config(path)?;
    if let Some(value) = set {
        if value.trim().is_empty() {
            return Err("pairing secret cannot be empty".into());
        }
        config.pairing_secret = Some(value);
        save_config(path, &config)?;
        println!("Pairing secret: set.");
        return Ok(());
    }
    if clear {
        config.pairing_secret = None;
        save_config(path, &config)?;
        println!("Pairing secret: cleared.");
        return Ok(());
    }

    let status = if config.pairing_secret.is_some() {
        "set"
    } else {
        "not set"
    };
    println!("Pairing secret: {status}.");
    Ok(())
}

fn listen_device(
    path: &Path,
    listen: SocketAddr,
    auto_accept: bool,
    auto_approve: bool,
    auto_approve_minutes: u64,
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
        auto_accept: {
            let mut cfg = config.lock().expect("config lock");
            let (active, expired) = cfg.auto_approve_state();
            if expired {
                let _ = save_config(path, &cfg);
            }
            auto_accept || auto_approve || active
        },
    });

    let mut engine = Engine::new(
        EngineConfig::new(identity.clone()).with_listen_addr(listen),
        state,
        handler,
    );

    let adapter_ids = {
        let cfg = config.lock().expect("config lock");
        cfg.adapter_ids()
    };

    for adapter_id in &adapter_ids {
        let adapter = {
            let cfg = config.lock().expect("config lock");
            build_adapter(adapter_id, &cfg)?
        };
        engine.register_adapter(adapter)?;
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

    let mut adapter_watches = Vec::new();
    if !adapter_ids.is_empty() {
        let state_path = config.lock().expect("config lock").state_path.clone();
        for adapter_id in &adapter_ids {
            let watch = engine.watch(adapter_id, state_path.clone(), Duration::from_millis(250))?;
            adapter_watches.push(watch);
        }
    }

    println!(
        "Listening on {} (device: {}, user: {})",
        listener_addr, identity.device_id, identity.user_id
    );

    let deadline = duration_secs.map(|secs| Instant::now() + Duration::from_secs(secs));
    let session_auto_approve_until = if auto_approve {
        Some(now_unix_secs() + auto_approve_minutes.saturating_mul(60))
    } else {
        None
    };
    let mut auto_approve_last = Instant::now()
        .checked_sub(Duration::from_secs(AUTO_APPROVE_INTERVAL_SECS))
        .unwrap_or_else(Instant::now);
    let mut auto_approve_attempts: HashMap<String, Instant> = HashMap::new();

    while running.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(500));
        let auto_approve_enabled = {
            let mut cfg = config.lock().expect("config lock");
            let (active, expired) = cfg.auto_approve_state();
            if expired {
                let _ = save_config(path, &cfg);
            }
            let session_active = session_auto_approve_until
                .map(|until| now_unix_secs() <= until)
                .unwrap_or(false);
            active || session_active
        };
        if auto_approve_enabled
            && !no_discovery
            && auto_approve_last.elapsed() >= Duration::from_secs(AUTO_APPROVE_INTERVAL_SECS)
        {
            if let Err(error) =
                auto_approve_devices(&engine, &config, path, &mut auto_approve_attempts)
            {
                eprintln!("Auto-approve error: {error}");
            }
            auto_approve_last = Instant::now();
        }
        if let Some(deadline) = deadline {
            if Instant::now() >= deadline {
                break;
            }
        }
    }

    let listener = engine.stop_listening();
    running.store(false, Ordering::SeqCst);
    for watch in adapter_watches {
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
    auto_approve: bool,
    auto_approve_minutes: u64,
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
    if auto_approve {
        cmd.arg("--auto-approve");
        cmd.arg("--auto-approve-minutes")
            .arg(auto_approve_minutes.to_string());
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
    adapter_id: Option<&str>,
    all_adapters: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_config(path)?;
    let device = resolve_device_address(&config, device, device_id.as_deref())?;
    let adapter_ids = resolve_adapter_ids(&config, adapter_id, all_adapters)?;

    let device_keys = config.device_keys()?;
    let app_key = config.app_key()?;
    let state = load_or_init_state(&config)?;
    let handler = Arc::new(StaticHandler {
        app_id: config.app_id.clone(),
        allowed: allowed_fingerprint_map(&config),
        keys: device_keys,
        app_key: app_key.clone(),
    });
    let mut engine = Engine::new(EngineConfig::new(config.identity()), state, handler);

    for adapter_id in &adapter_ids {
        let adapter = build_adapter(adapter_id, &config)?;
        engine.register_adapter(adapter)?;
    }

    let mut remote_device = None::<DeviceInfo>;
    for adapter_id in &adapter_ids {
        let device_info = engine.sync_now(device, adapter_id)?;
        remote_device = Some(device_info);
    }

    let state = engine.state();
    state
        .lock()
        .expect("state lock")
        .save_encrypted(&app_key, &config.state_path)?;
    if let Some(remote_device) = remote_device {
        config.upsert_device(
            &remote_device.identity,
            Some(device),
            remote_device.fingerprint.clone(),
        );
    }
    save_config(path, &config)?;

    println!("Refreshed {} adapter(s) with {}", adapter_ids.len(), device);

    Ok(())
}

fn refresh_all(
    path: &Path,
    discover: bool,
    adapter_id: Option<&str>,
    all_adapters: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_config(path)?;
    if config.devices.is_empty() {
        println!("No linked devices to refresh.");
        return Ok(());
    }

    let adapter_ids = resolve_adapter_ids(&config, adapter_id, all_adapters)?;
    let changed = refresh_all_devices(path, &mut config, discover, &adapter_ids)?;
    if changed {
        println!("Refresh complete; local file updated.");
    } else {
        println!("Refresh complete; no local changes applied.");
    }
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
    let (auto_active, auto_expired) = config.auto_approve_state();
    if auto_expired {
        save_config(path, &config)?;
    }
    if auto_active {
        if let Some(until) = config.auto_approve_until {
            println!(
                "Auto-approve: enabled ({})",
                format_time_label("expires", until)
            );
        } else {
            println!("Auto-approve: enabled (no expiry)");
        }
    } else {
        println!("Auto-approve: disabled");
    }
    if let Some(keys) = config.device_keys.as_ref() {
        println!("Fingerprint: {}", keys.fingerprint);
    }
    println!(
        "Pairing secret: {}",
        if config.pairing_secret.is_some() {
            "set"
        } else {
            "not set"
        }
    );
    let adapter_ids = config.adapter_ids();
    if adapter_ids.is_empty() {
        println!("Selected adapters: (none)");
    } else {
        let default_id = config.default_adapter_id();
        println!("Selected adapters:");
        for adapter_id in adapter_ids {
            let adapter = config.adapter_config(&adapter_id);
            let kind = adapter
                .as_ref()
                .map(|config| format!("{:?}", config.kind).to_lowercase())
                .unwrap_or_else(|| "unknown".to_string());
            let path = adapter
                .map(|config| config.path.display().to_string())
                .unwrap_or_else(|| "(missing)".to_string());
            let marker = if default_id.as_deref() == Some(adapter_id.as_str()) {
                " (default)"
            } else {
                ""
            };
            println!("  {} ({}) : {}{}", adapter_id, kind, path, marker);
        }
    }
    print_listener_status(path)?;
    print_backup_status(&config, path)?;
    print_storage_status(&config)?;

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
                let linked = config.devices.contains_key(&device.identity.device_id);
                let status = if linked { "linked" } else { "unlinked" };
                let address = device
                    .address
                    .map(|addr| addr.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                println!(
                    "  {} ({}) at {} [{}]",
                    device.identity.device_id, device.identity.user_id, address, status
                );
            }
        }
    }

    if !config.devices.is_empty() {
        println!("\nLinked devices:");
    } else {
        println!("\nLinked devices: (none)");
    }

    for record in config.devices.values_mut() {
        if let Some(found) = discovered_map.get(&record.device_id) {
            if let Some(address) = found.address {
                record.last_seen_addr = Some(address.to_string());
                record.last_seen_unix_secs = Some(now_unix_secs());
            }
        }
        let addr = record.last_seen_addr.as_deref().unwrap_or("unknown");
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

fn diagnose(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let config = load_config(path)?;
    println!("Config: {}", path.display());
    println!("Device ID: {}", config.device_id);
    println!("App ID: {}", config.app_id);
    println!("User ID: {}", config.user_id);

    match config.device_keys() {
        Ok(keys) => println!("Device keys: OK (fingerprint {})", keys.fingerprint()),
        Err(error) => println!("Device keys: ERROR ({error})"),
    }

    match config.app_key() {
        Ok(_) => println!("App key: OK"),
        Err(error) => println!("App key: ERROR ({error})"),
    }

    if config.state_path.exists() {
        match State::load_maybe_encrypted(&config.app_key()?, &config.state_path) {
            Ok(state) => println!("State file: OK (entries {})", state.entries.len()),
            Err(error) => println!("State file: ERROR ({error})"),
        }
    } else {
        println!("State file: missing (will be created on first refresh)");
    }

    let adapter_ids = config.adapter_ids();
    if adapter_ids.is_empty() {
        println!("Selected adapters: (none)");
    } else {
        println!("Selected adapters:");
        for adapter_id in adapter_ids {
            let adapter = config.adapter_config(&adapter_id);
            let kind = adapter
                .as_ref()
                .map(|config| format!("{:?}", config.kind).to_lowercase())
                .unwrap_or_else(|| "unknown".to_string());
            let path = adapter.map(|config| config.path);
            match path {
                Some(path) if path.exists() => {
                    println!("  {adapter_id} ({kind}): OK ({})", path.display());
                }
                Some(path) => {
                    println!("  {adapter_id} ({kind}): missing ({})", path.display());
                }
                None => println!("  {adapter_id} ({kind}): missing"),
            }
        }
    }

    println!("Backups enabled: {}", config.backup_enabled);
    println!("Restore allowed: {}", config.backup_allow_restore);
    if config.backup_enabled || config.backup_dir.is_some() {
        println!("Backup dir: {}", config.backup_dir(path).display());
    }

    if config.devices.is_empty() {
        println!("Linked devices: (none)");
    } else {
        println!("Linked devices: {}", config.devices.len());
    }

    Ok(())
}

fn print_backup_status(
    config: &Config,
    config_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Backups enabled: {}", config.backup_enabled);
    println!("Restore allowed: {}", config.backup_allow_restore);
    if config.backup_enabled || config.backup_dir.is_some() {
        let backup_dir = config.backup_dir(config_path);
        println!("Backup dir: {}", backup_dir.display());
        match dir_size(&backup_dir) {
            Ok(size) => println!("Backup size: {}", format_bytes(size)),
            Err(_) => println!("Backup size: unknown"),
        }
    }
    Ok(())
}

fn print_storage_status(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let state_path = &config.state_path;
    match file_size(state_path) {
        Some(size) => println!(
            "State file: {} ({})",
            state_path.display(),
            format_bytes(size)
        ),
        None => println!("State file: {} (missing)", state_path.display()),
    }
    if let Some(updated) = file_modified_unix_secs(state_path) {
        println!("{}", format_time_label("State updated", updated));
    }

    let adapter_ids = config.adapter_ids();
    if adapter_ids.is_empty() {
        println!("Selected adapters: (none)");
    } else {
        for adapter_id in adapter_ids {
            let adapter = config.adapter_config(&adapter_id);
            let kind = adapter
                .as_ref()
                .map(|config| adapter_kind_label(config.kind).to_string())
                .unwrap_or_else(|| "unknown".to_string());
            if let Some(path) = adapter.map(|config| config.path) {
                match file_size(&path) {
                    Some(size) => println!(
                        "Adapter {} ({}) size: {} ({})",
                        adapter_id,
                        kind,
                        path.display(),
                        format_bytes(size)
                    ),
                    None => println!(
                        "Adapter {} ({}) size: {} (missing)",
                        adapter_id,
                        kind,
                        path.display()
                    ),
                }
                if let Some(updated) = file_modified_unix_secs(&path) {
                    println!(
                        "{}",
                        format_time_label(
                            &format!("Adapter {} ({}) updated", adapter_id, kind),
                            updated
                        )
                    );
                }
            }
        }
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
            println!("Listener: unknown (failed to read PID file: {})", error);
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

    let mut changed = false;

    if config.device_keys.is_none() {
        let identity = config.identity();
        let keys =
            DeviceKeys::generate(&identity).map_err(|error| io::Error::other(error.to_string()))?;
        config.device_keys = Some(DeviceKeysRecord::from_keys(&keys));
        changed = true;
    }

    if config.app_key.is_none() {
        let app_key = AppKey::generate().map_err(|error| io::Error::other(error.to_string()))?;
        config.set_app_key(&app_key);
        changed = true;
    }

    if config.adapters.is_empty() {
        if !config.data_paths.is_empty() {
            for (adapter_id, path) in config.data_paths.clone() {
                config.adapters.insert(
                    adapter_id,
                    AdapterConfig {
                        kind: AdapterKindConfig::Json,
                        path,
                        page_delta: None,
                        namespace: None,
                    },
                );
            }
            changed = true;
        } else if let Some(path) = config.data_path.clone() {
            config.adapters.insert(
                FILE_KEY.to_string(),
                AdapterConfig {
                    kind: AdapterKindConfig::Json,
                    path,
                    page_delta: None,
                    namespace: None,
                },
            );
            changed = true;
        }
    }

    if config.data_paths.is_empty() {
        if let Some(path) = config.data_path.clone() {
            config.data_paths.insert(FILE_KEY.to_string(), path);
            changed = true;
        }
    }

    if config.default_adapter.is_none() {
        if config.adapters.contains_key(FILE_KEY) || config.data_paths.contains_key(FILE_KEY) {
            config.default_adapter = Some(FILE_KEY.to_string());
            changed = true;
        } else if let Some(first) = config.adapters.keys().next().cloned() {
            config.default_adapter = Some(first);
            changed = true;
        } else if let Some(first) = config.data_paths.keys().next().cloned() {
            config.default_adapter = Some(first);
            changed = true;
        }
    }

    if config.auto_approve {
        let (_active, expired) = config.auto_approve_state();
        if expired {
            changed = true;
        }
    }

    if changed {
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

fn ensure_file(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("failed to create directory {}: {error}", parent.display()),
            )
        })?;
    }
    fs::write(path, b"").map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("failed to write file {}: {error}", path.display()),
        )
    })?;
    Ok(())
}

fn ensure_json_file(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "failed to create data directory {}: {error}",
                    parent.display()
                ),
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

fn ensure_logical_file(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "failed to create data directory {}: {error}",
                    parent.display()
                ),
            )
        })?;
    }
    fs::write(path, b"[]").map_err(|error| {
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
    let stored_devices = stored_device_infos(config);

    if let Some(device_id) = device_id {
        let matched = devices
            .into_iter()
            .find(|device| device.identity.device_id == device_id)
            .and_then(|device| device.address);
        if let Some(address) = matched {
            return Ok(address);
        }
        if let Some(address) = stored_devices
            .iter()
            .find(|device| device.identity.device_id == device_id)
            .and_then(|device| device.address)
        {
            return Ok(address);
        }
        return Err(format!("device not found: {device_id}").into());
    }

    if devices.len() == 1 {
        return devices[0]
            .address
            .ok_or_else(|| "device address unavailable".into());
    }

    if devices.is_empty() && !stored_devices.is_empty() {
        if stored_devices.len() == 1 {
            return stored_devices[0]
                .address
                .ok_or_else(|| "device address unavailable".into());
        }
        let selection = prompt_select_device(&stored_devices)?;
        return selection
            .address
            .ok_or_else(|| "device address unavailable".into());
    }

    let selection = prompt_select_device(&devices)?;
    selection
        .address
        .ok_or_else(|| "device address unavailable".into())
}

fn stored_device_infos(config: &Config) -> Vec<DeviceInfo> {
    config
        .devices
        .values()
        .filter_map(|record| {
            let address = record
                .last_seen_addr
                .as_deref()
                .and_then(|addr| addr.parse::<SocketAddr>().ok());
            address.map(|address| DeviceInfo {
                identity: Identity::new(&record.device_id, &record.app_id, &record.user_id),
                address: Some(address),
                last_seen: record
                    .last_seen_unix_secs
                    .map(|secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs)),
                linked: true,
                fingerprint: record.fingerprint.clone(),
            })
        })
        .collect()
}

fn resolve_adapter_ids(
    config: &Config,
    adapter_id: Option<&str>,
    all_adapters: bool,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    if all_adapters {
        let ids = config.adapter_ids();
        if ids.is_empty() {
            return Err("no adapters selected; run libresync select".into());
        }
        return Ok(ids);
    }

    if let Some(adapter_id) = adapter_id {
        return Ok(vec![adapter_id.to_string()]);
    }

    if let Some(default_id) = config.default_adapter_id() {
        return Ok(vec![default_id]);
    }

    Err("no adapters selected; run libresync select".into())
}

fn adapter_config_or_err(
    config: &Config,
    adapter_id: &str,
) -> Result<AdapterConfig, Box<dyn std::error::Error>> {
    config
        .adapter_config(adapter_id)
        .ok_or_else(|| format!("no adapter configured for {adapter_id}").into())
}

fn adapter_kind_label(kind: AdapterKindConfig) -> &'static str {
    match kind {
        AdapterKindConfig::Json => "json",
        AdapterKindConfig::Sqlite => "sqlite",
        AdapterKindConfig::LogicalFile => "logical-file",
    }
}

fn build_adapter(
    adapter_id: &str,
    config: &Config,
) -> Result<Arc<dyn DataAdapter>, Box<dyn std::error::Error>> {
    let adapter = adapter_config_or_err(config, adapter_id)?;
    match adapter.kind {
        AdapterKindConfig::Json => Ok(Arc::new(JsonFileAdapter::new(adapter_id, adapter.path))),
        AdapterKindConfig::Sqlite => {
            let mut sqlite = SqliteFileAdapter::new(adapter_id, &adapter.path);
            if let Some(delta) = adapter.page_delta {
                sqlite = sqlite.with_page_delta(delta);
            }
            Ok(Arc::new(sqlite))
        }
        AdapterKindConfig::LogicalFile => {
            let namespace = adapter.namespace.unwrap_or_else(|| config.app_id.clone());
            let logical = FileLogicalAdapter::new(adapter_id, namespace, adapter.path);
            Ok(Arc::new(LogicalAdapterWrapper::new(Arc::new(logical))))
        }
    }
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

fn file_size(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().map(|metadata| metadata.len())
}

fn file_modified_unix_secs(path: &Path) -> Option<u64> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let duration = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(duration.as_secs())
}

fn dir_size(path: &Path) -> io::Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    if !path.is_dir() {
        return Ok(fs::metadata(path)?.len());
    }

    let mut total = 0u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let entry_path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            total = total.saturating_add(dir_size(&entry_path)?);
        } else if file_type.is_file() {
            total = total.saturating_add(entry.metadata()?.len());
        }
    }
    Ok(total)
}

fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    let value = bytes as f64;
    if value >= GB {
        format!("{:.1} GB", value / GB)
    } else if value >= MB {
        format!("{:.1} MB", value / MB)
    } else if value >= KB {
        format!("{:.1} KB", value / KB)
    } else {
        format!("{bytes} B")
    }
}

fn format_time_label(label: &str, ts: u64) -> String {
    let now = now_unix_secs() as i64;
    let ts_i64 = ts as i64;
    let delta = now.saturating_sub(ts_i64);
    let relative = format_relative(delta);

    match Local.timestamp_opt(ts_i64, 0).single() {
        Some(local) => format!(
            "{label}: {} ({})",
            local.format("%Y-%m-%d %H:%M:%S"),
            relative
        ),
        None => format!("{label}: {ts} ({relative})"),
    }
}

fn format_last_seen(ts: u64) -> String {
    format_time_label("last seen", ts)
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

#[allow(clippy::too_many_arguments)]
fn watch_file(
    path: &Path,
    adapter_id: Option<&str>,
    all_adapters: bool,
    listen: SocketAddr,
    interval_secs: u64,
    debounce_ms: u64,
    no_discover: bool,
    no_listen: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut config = load_config(path)?;
    let adapter_ids = resolve_adapter_ids(&config, adapter_id, all_adapters)?;
    let mut watch_paths = Vec::new();
    for adapter_id in &adapter_ids {
        let adapter = adapter_config_or_err(&config, adapter_id)?;
        match adapter.kind {
            AdapterKindConfig::Json => ensure_json_file(&adapter.path)?,
            AdapterKindConfig::Sqlite => ensure_file(&adapter.path)?,
            AdapterKindConfig::LogicalFile => ensure_logical_file(&adapter.path)?,
        }
        watch_paths.push((adapter_id.clone(), adapter.path));
    }

    if watch_paths.len() == 1 {
        println!("Watching {}", watch_paths[0].1.display());
    } else {
        println!("Watching {} adapters:", watch_paths.len());
        for (adapter_id, path) in &watch_paths {
            println!("  {adapter_id}: {}", path.display());
        }
    }
    println!(
        "Linked devices: {}",
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
    for (_, path) in &watch_paths {
        watcher.watch(path, RecursiveMode::NonRecursive)?;
    }

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
                println!("Change detected at {timestamp}; refreshing linked devices.");
            }
            let refresh_result = refresh_all_devices(path, &mut config, !no_discover, &adapter_ids);
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

    let config = load_config(path)?;
    let auto_approve = config.auto_approve;
    match spawn_background_listener(path, listen, false, auto_approve, 15, false, None) {
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

fn is_auto_approve_address(addr: SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(ip) => ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_unique_local() || ip.is_unicast_link_local(),
    }
}

fn auto_approve_devices(
    engine: &Engine,
    config: &Arc<Mutex<Config>>,
    path: &Path,
    attempts: &mut HashMap<String, Instant>,
) -> Result<(), Box<dyn std::error::Error>> {
    let devices = engine
        .discover_devices_with_timeout(Duration::from_secs(AUTO_APPROVE_DISCOVER_TIMEOUT_SECS))?;
    if devices.is_empty() {
        return Ok(());
    }

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
        let already_linked = {
            let cfg = config.lock().expect("config lock");
            cfg.is_linked(&device_id)
        };
        if already_linked {
            continue;
        }
        if let Some(last) = attempts.get(&device_id) {
            if now.duration_since(*last) < Duration::from_secs(AUTO_APPROVE_RETRY_SECS) {
                continue;
            }
        }

        match engine.request_link(addr) {
            Ok(remote) => {
                let mut cfg = config.lock().expect("config lock");
                cfg.upsert_device(&remote.identity, Some(addr), remote.fingerprint.clone());
                save_config(path, &cfg)?;
                println!(
                    "Auto-approved link with {} ({})",
                    remote.identity.device_id, remote.identity.user_id
                );
                attempts.remove(&device_id);
            }
            Err(error) => {
                attempts.insert(device_id, now);
                eprintln!("Auto-approve link failed: {error}");
            }
        }
    }

    Ok(())
}

fn refresh_all_devices(
    path: &Path,
    config: &mut Config,
    discover: bool,
    adapter_ids: &[String],
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
            match refresh_with_address(path, config, addr, adapter_ids) {
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
                    eprintln!("Refresh error for {device_id}: {error} (is the listener running?)");
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
    adapter_ids: &[String],
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut adapter_paths = Vec::new();
    for adapter_id in adapter_ids {
        adapter_paths.push(adapter_config_or_err(config, adapter_id)?.path);
    }

    let mut before_bytes = Vec::new();
    for path in &adapter_paths {
        before_bytes.extend(fs::read(path).unwrap_or_default());
    }

    let device_keys = config.device_keys()?;
    let app_key = config.app_key()?;
    let state = load_or_init_state(config)?;
    let handler = Arc::new(StaticHandler {
        app_id: config.app_id.clone(),
        allowed: allowed_fingerprint_map(config),
        keys: device_keys,
        app_key: app_key.clone(),
    });
    let mut engine = Engine::new(EngineConfig::new(config.identity()), state, handler);
    for adapter_id in adapter_ids {
        let adapter = build_adapter(adapter_id, config)?;
        engine.register_adapter(adapter)?;
    }

    let mut remote_device = None::<DeviceInfo>;
    for adapter_id in adapter_ids {
        let device_info = engine.sync_now(device, adapter_id)?;
        remote_device = Some(device_info);
    }

    let mut after_bytes = Vec::new();
    for path in &adapter_paths {
        after_bytes.extend(fs::read(path).unwrap_or_default());
    }
    let changed = before_bytes != after_bytes;

    let state = engine.state();
    state
        .lock()
        .expect("state lock")
        .save_encrypted(&app_key, &config.state_path)?;
    if let Some(remote_device) = remote_device {
        config.upsert_device(
            &remote_device.identity,
            Some(device),
            remote_device.fingerprint.clone(),
        );
    }
    save_config(path, config)?;
    Ok(changed)
}

fn generate_device_id() -> String {
    let mut rng = rand::rng();
    let words = DEVICE_WORDS
        .choose_multiple(&mut rng, 3)
        .cloned()
        .collect::<Vec<_>>();
    words.join("-")
}

fn generate_user_id() -> String {
    let mut rng = rand::rng();
    let adjective = ADJECTIVES.choose(&mut rng).unwrap_or(&"calm");
    let noun = NOUNS.choose(&mut rng).unwrap_or(&"forest");
    format!("{}-{}", adjective, noun)
}

const DEVICE_WORDS: &[&str] = &[
    "amber", "anchor", "atlas", "aurora", "blossom", "breeze", "canyon", "cedar", "cliff", "comet",
    "coral", "cove", "dawn", "delta", "ember", "fable", "field", "fjord", "forest", "glade",
    "harbor", "haven", "island", "keystone", "lagoon", "lumen", "meadow", "mesa", "mist", "nova",
    "orbit", "pine", "prairie", "ridge", "river", "sage", "sierra", "signal", "sky", "solace",
    "spark", "stone", "summit", "tide", "vale", "valley", "vista", "wild", "zephyr",
];

const ADJECTIVES: &[&str] = &[
    "brisk", "calm", "clear", "cozy", "gentle", "glad", "golden", "grand", "kind", "lively",
    "mellow", "neat", "nimble", "proud", "quiet", "steady", "swift", "tender", "true", "vivid",
    "warm",
];

const NOUNS: &[&str] = &[
    "brook", "cascade", "canyon", "cloud", "crest", "dune", "field", "forest", "garden", "grove",
    "harbor", "island", "meadow", "orchard", "path", "peak", "prairie", "ridge", "river", "signal",
    "summit", "trail", "vale",
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
        let device_keys =
            DeviceKeysRecord::from_keys(&DeviceKeys::generate(&identity).expect("device keys"));
        let app_key = AppKey::generate().expect("app key");
        let config = Config {
            device_id: "local-device".to_string(),
            app_id: APP_ID_DEFAULT.to_string(),
            user_id: "user".to_string(),
            state_path: PathBuf::from("state.json"),
            data_path: None,
            data_paths: BTreeMap::new(),
            default_adapter: None,
            adapters: BTreeMap::new(),
            device_keys: Some(device_keys),
            app_key: Some(BASE64.encode(app_key.as_bytes())),
            backup_enabled: false,
            backup_allow_restore: false,
            backup_dir: None,
            auto_approve: false,
            auto_approve_until: None,
            pairing_secret: None,
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
        let device_keys =
            DeviceKeysRecord::from_keys(&DeviceKeys::generate(&identity).expect("device keys"));
        let app_key = AppKey::generate().expect("app key");
        let config = Config {
            device_id: "local-device".to_string(),
            app_id: APP_ID_DEFAULT.to_string(),
            user_id: "user".to_string(),
            state_path: PathBuf::from("state.json"),
            data_path: None,
            data_paths: BTreeMap::new(),
            default_adapter: None,
            adapters: BTreeMap::new(),
            device_keys: Some(device_keys),
            app_key: Some(BASE64.encode(app_key.as_bytes())),
            backup_enabled: false,
            backup_allow_restore: false,
            backup_dir: None,
            auto_approve: false,
            auto_approve_until: None,
            pairing_secret: None,
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
    fn resolve_device_address_uses_stored_address() {
        let identity = Identity::new("local-device", APP_ID_DEFAULT, "user");
        let device_keys =
            DeviceKeysRecord::from_keys(&DeviceKeys::generate(&identity).expect("device keys"));
        let app_key = AppKey::generate().expect("app key");
        let mut config = Config {
            device_id: "local-device".to_string(),
            app_id: APP_ID_DEFAULT.to_string(),
            user_id: "user".to_string(),
            state_path: PathBuf::from("state.json"),
            data_path: None,
            data_paths: BTreeMap::new(),
            default_adapter: None,
            adapters: BTreeMap::new(),
            device_keys: Some(device_keys),
            app_key: Some(BASE64.encode(app_key.as_bytes())),
            backup_enabled: false,
            backup_allow_restore: false,
            backup_dir: None,
            auto_approve: false,
            auto_approve_until: None,
            pairing_secret: None,
            devices: BTreeMap::new(),
        };
        let remote_identity = Identity::new("remote-device", APP_ID_DEFAULT, "remote-user");
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 60000));
        config.upsert_device(&remote_identity, Some(addr), None);

        let resolved = resolve_device_address_with_timeout(
            &config,
            None,
            Some("remote-device"),
            Duration::from_millis(1),
        )
        .expect("resolve");
        assert_eq!(resolved, addr);
    }

    #[test]
    fn set_device_address_updates_last_seen() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        let mut config = load_config(&config_path).expect("load");
        let identity = Identity::new("remote", APP_ID_DEFAULT, "remote-user");
        config.upsert_device(&identity, None, None);
        save_config(&config_path, &config).expect("save");

        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 52345));
        set_device_address(&config_path, "remote", addr).expect("set address");

        let updated = load_config(&config_path).expect("load");
        let record = updated.devices.get("remote").expect("record");
        assert_eq!(record.last_seen_addr.as_deref(), Some("127.0.0.1:52345"));
        assert!(record.last_seen_unix_secs.unwrap_or(0) > 0);
    }

    #[test]
    fn upsert_device_sets_last_seen_fields() {
        let identity = Identity::new("local", APP_ID_DEFAULT, "user");
        let device_keys =
            DeviceKeysRecord::from_keys(&DeviceKeys::generate(&identity).expect("device keys"));
        let app_key = AppKey::generate().expect("app key");
        let mut config = Config {
            device_id: "local".to_string(),
            app_id: APP_ID_DEFAULT.to_string(),
            user_id: "user".to_string(),
            state_path: PathBuf::from("state.json"),
            data_path: None,
            data_paths: BTreeMap::new(),
            default_adapter: None,
            adapters: BTreeMap::new(),
            device_keys: Some(device_keys),
            app_key: Some(BASE64.encode(app_key.as_bytes())),
            backup_enabled: false,
            backup_allow_restore: false,
            backup_dir: None,
            auto_approve: false,
            auto_approve_until: None,
            pairing_secret: None,
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
    fn auto_approve_state_expires_and_clears() {
        let identity = Identity::new("local", APP_ID_DEFAULT, "user");
        let device_keys =
            DeviceKeysRecord::from_keys(&DeviceKeys::generate(&identity).expect("device keys"));
        let app_key = AppKey::generate().expect("app key");
        let mut config = Config {
            device_id: "local".to_string(),
            app_id: APP_ID_DEFAULT.to_string(),
            user_id: "user".to_string(),
            state_path: PathBuf::from("state.json"),
            data_path: None,
            data_paths: BTreeMap::new(),
            default_adapter: None,
            adapters: BTreeMap::new(),
            device_keys: Some(device_keys),
            app_key: Some(BASE64.encode(app_key.as_bytes())),
            backup_enabled: false,
            backup_allow_restore: false,
            backup_dir: None,
            auto_approve: true,
            auto_approve_until: Some(now_unix_secs().saturating_sub(1)),
            pairing_secret: None,
            devices: BTreeMap::new(),
        };

        let (active, expired) = config.auto_approve_state();
        assert!(!active);
        assert!(expired);
        assert!(!config.auto_approve);
        assert!(config.auto_approve_until.is_none());
    }

    #[test]
    fn set_auto_approve_updates_config() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        set_auto_approve(&config_path, true, false, 1, false).expect("enable");
        let config = load_config(&config_path).expect("load");
        assert!(config.auto_approve);
        assert!(config.auto_approve_until.is_some());

        set_auto_approve(&config_path, true, false, 5, true).expect("enable persist");
        let config = load_config(&config_path).expect("load");
        assert!(config.auto_approve);
        assert!(config.auto_approve_until.is_none());

        set_auto_approve(&config_path, false, true, 5, false).expect("disable");
        let config = load_config(&config_path).expect("load");
        assert!(!config.auto_approve);
        assert!(config.auto_approve_until.is_none());
    }

    #[test]
    fn set_auto_approve_rejects_conflict() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        let error =
            set_auto_approve(&config_path, true, true, 1, false).expect_err("expected error");
        assert!(error.to_string().contains("choose either"));
    }

    #[test]
    fn set_pairing_secret_sets_and_clears() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        set_pairing_secret(&config_path, Some("shared-secret".to_string()), false)
            .expect("set secret");
        let config = load_config(&config_path).expect("load");
        assert_eq!(config.pairing_secret.as_deref(), Some("shared-secret"));

        set_pairing_secret(&config_path, None, true).expect("clear secret");
        let config = load_config(&config_path).expect("load");
        assert!(config.pairing_secret.is_none());
    }

    #[test]
    fn set_pairing_secret_rejects_empty() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        let error = set_pairing_secret(&config_path, Some("  ".to_string()), false)
            .expect_err("expected error");
        assert!(error.to_string().contains("pairing secret cannot be empty"));
    }

    #[test]
    fn backup_snapshot_list_restore_round_trip() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let data_path = temp.path().join("data.json");

        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");
        select_file(
            &config_path,
            FILE_KEY,
            AdapterKindConfig::Json,
            None,
            None,
            &data_path,
        )
        .expect("select");
        configure_backups(&config_path, true, true, None).expect("configure");
        std::fs::write(&data_path, b"{\"before\":true}").expect("write");

        create_backup_snapshot(&config_path, FILE_KEY, Some("test".to_string())).expect("snapshot");

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

        preview_backup_snapshot(&config_path, FILE_KEY, &snapshots[0].id).expect("preview");

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
    fn refresh_all_handles_no_devices() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        refresh_all(&config_path, false, None, false).expect("refresh all");
    }

    #[test]
    fn status_helpers_render_storage_and_backup_info() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let data_path = temp.path().join("data.json");

        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");
        select_file(
            &config_path,
            FILE_KEY,
            AdapterKindConfig::Json,
            None,
            None,
            &data_path,
        )
        .expect("select");
        configure_backups(&config_path, true, false, None).expect("configure");
        std::fs::write(&data_path, b"{\"alpha\":1}").expect("write");

        let config = load_config(&config_path).expect("load");
        print_backup_status(&config, &config_path).expect("backup status");
        print_storage_status(&config).expect("storage status");
    }

    #[test]
    fn prune_backup_snapshots_dry_run() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let data_path = temp.path().join("data.json");

        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");
        select_file(
            &config_path,
            FILE_KEY,
            AdapterKindConfig::Json,
            None,
            None,
            &data_path,
        )
        .expect("select");
        configure_backups(&config_path, true, false, None).expect("configure");

        create_backup_snapshot(&config_path, FILE_KEY, None).expect("snapshot");
        prune_backup_snapshots(&config_path, FILE_KEY, Some(1), None, true).expect("prune");
    }

    #[test]
    fn rotate_app_key_requires_confirm() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        let result = rotate_app_key(&config_path, false, false);
        assert!(result.is_err());
    }

    #[test]
    fn rotate_app_key_clears_allowlist_by_default() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");
        let mut config = load_config(&config_path).expect("load");
        config.devices.insert(
            "device-a".to_string(),
            DeviceRecord {
                device_id: "device-a".to_string(),
                user_id: "user-a".to_string(),
                app_id: APP_ID_DEFAULT.to_string(),
                last_seen_addr: None,
                last_seen_unix_secs: None,
                fingerprint: None,
            },
        );
        save_config(&config_path, &config).expect("save");

        rotate_app_key(&config_path, false, true).expect("rotate");
        let config = load_config(&config_path).expect("reload");
        assert!(config.devices.is_empty());
    }

    #[test]
    fn export_app_key_writes_file() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let output = temp.path().join("app.key");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        export_app_key(&config_path, &output).expect("export");
        let raw = std::fs::read_to_string(&output).expect("read");
        let bytes = BASE64.decode(raw.trim().as_bytes()).expect("decode");
        let key = AppKey::from_slice(&bytes).expect("key");

        let config = load_config(&config_path).expect("load");
        assert_eq!(config.app_key().expect("app key"), key);
    }

    #[test]
    fn import_app_key_requires_confirm() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let input = temp.path().join("app.key");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        let new_key = AppKey::generate().expect("new key");
        std::fs::write(&input, BASE64.encode(new_key.as_bytes())).expect("write");

        let result = import_app_key(&config_path, &input, false, false);
        assert!(result.is_err());
    }

    #[test]
    fn export_device_keys_writes_file() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let output = temp.path().join("device.keys");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        export_device_keys(&config_path, &output).expect("export");
        let data = std::fs::read(&output).expect("read");
        let record: DeviceKeysRecord = serde_json::from_slice(&data).expect("parse");
        assert!(!record.fingerprint.is_empty());
    }

    #[test]
    fn import_device_keys_updates_fingerprint() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let input = temp.path().join("device.keys");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        let identity = Identity::new("device", APP_ID_DEFAULT, "user");
        let keys = DeviceKeys::generate(&identity).expect("device keys");
        let record = DeviceKeysRecord::from_keys(&keys);
        let data = serde_json::to_vec_pretty(&record).expect("serialize");
        std::fs::write(&input, data).expect("write");

        import_device_keys(&config_path, &input, false, true).expect("import");

        let config = load_config(&config_path).expect("load");
        let fingerprint = config
            .device_keys
            .as_ref()
            .expect("device keys")
            .fingerprint
            .clone();
        assert_eq!(fingerprint, keys.fingerprint());
    }

    #[test]
    fn rotate_device_keys_requires_confirm() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        let result = rotate_device_keys(&config_path, false, false);
        assert!(result.is_err());
    }

    #[test]
    fn rotate_device_keys_updates_fingerprint() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        let before = load_config(&config_path).expect("load");
        let before_fp = before
            .device_keys
            .as_ref()
            .expect("device keys")
            .fingerprint
            .clone();

        rotate_device_keys(&config_path, false, true).expect("rotate");

        let after = load_config(&config_path).expect("load");
        let after_fp = after
            .device_keys
            .as_ref()
            .expect("device keys")
            .fingerprint
            .clone();
        assert_ne!(before_fp, after_fp);
    }

    #[test]
    fn diagnose_runs() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");

        diagnose(&config_path).expect("diagnose");
    }

    #[test]
    fn backup_restore_requires_allow_restore() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let data_path = temp.path().join("data.json");

        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");
        select_file(
            &config_path,
            FILE_KEY,
            AdapterKindConfig::Json,
            None,
            None,
            &data_path,
        )
        .expect("select");
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
        select_file(
            &config_path,
            FILE_KEY,
            AdapterKindConfig::Json,
            None,
            None,
            &data_path,
        )
        .expect("select");
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
            data_paths: BTreeMap::new(),
            default_adapter: None,
            adapters: BTreeMap::new(),
            device_keys: None,
            app_key: None,
            backup_enabled: false,
            backup_allow_restore: false,
            backup_dir: None,
            auto_approve: false,
            auto_approve_until: None,
            pairing_secret: None,
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
            data_paths: BTreeMap::new(),
            default_adapter: None,
            adapters: BTreeMap::new(),
            device_keys: None,
            app_key: None,
            backup_enabled: false,
            backup_allow_restore: false,
            backup_dir: None,
            auto_approve: false,
            auto_approve_until: None,
            pairing_secret: None,
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
            data_paths: BTreeMap::new(),
            default_adapter: None,
            adapters: BTreeMap::new(),
            device_keys: None,
            app_key: None,
            backup_enabled: false,
            backup_allow_restore: false,
            backup_dir: None,
            auto_approve: false,
            auto_approve_until: None,
            pairing_secret: None,
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
        let device_keys =
            DeviceKeysRecord::from_keys(&DeviceKeys::generate(&identity).expect("device keys"));
        let app_key = AppKey::generate().expect("app key");
        let mut config = Config {
            device_id: "local-device".to_string(),
            app_id: APP_ID_DEFAULT.to_string(),
            user_id: "user".to_string(),
            state_path: PathBuf::from("state.json"),
            data_path: None,
            data_paths: BTreeMap::new(),
            default_adapter: None,
            adapters: BTreeMap::new(),
            device_keys: Some(device_keys),
            app_key: Some(BASE64.encode(app_key.as_bytes())),
            backup_enabled: false,
            backup_allow_restore: false,
            backup_dir: None,
            auto_approve: false,
            auto_approve_until: None,
            pairing_secret: None,
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
        let error = io::Error::other("boom");
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
                id: FILE_KEY.to_string(),
                kind: AdapterKindArg::Json,
                page_delta: None,
                namespace: None,
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
                id: FILE_KEY.to_string(),
                kind: AdapterKindArg::Json,
                page_delta: None,
                namespace: None,
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
        let device_keys =
            DeviceKeysRecord::from_keys(&DeviceKeys::generate(&identity).expect("device keys"));
        let app_key = AppKey::generate().expect("app key");
        let mut config = Config {
            device_id: "local-device".to_string(),
            app_id: APP_ID_DEFAULT.to_string(),
            user_id: "user".to_string(),
            state_path: PathBuf::from("state.json"),
            data_path: None,
            data_paths: BTreeMap::new(),
            default_adapter: None,
            adapters: BTreeMap::new(),
            device_keys: Some(device_keys),
            app_key: Some(BASE64.encode(app_key.as_bytes())),
            backup_enabled: false,
            backup_allow_restore: false,
            backup_dir: None,
            auto_approve: false,
            auto_approve_until: None,
            pairing_secret: None,
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
    fn unlink_device_removes_and_errors_when_missing() {
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

        unlink_device(&config_path, "remote-device", true).expect("unlink");
        let config = load_config(&config_path).expect("reload");
        assert!(!config.devices.contains_key("remote-device"));

        let result = unlink_device(&config_path, "remote-device", true);
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
            data_paths: BTreeMap::new(),
            default_adapter: None,
            adapters: BTreeMap::new(),
            device_keys: None,
            app_key: None,
            backup_enabled: false,
            backup_allow_restore: false,
            backup_dir: None,
            auto_approve: false,
            auto_approve_until: None,
            pairing_secret: None,
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
        select_file(
            &config_path,
            FILE_KEY,
            AdapterKindConfig::Json,
            None,
            None,
            &data_path,
        )
        .expect("select");
        assert!(data_path.exists());
    }

    #[test]
    fn select_file_creates_logical_file() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let data_path = temp.path().join("records.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");
        select_file(
            &config_path,
            "records",
            AdapterKindConfig::LogicalFile,
            None,
            None,
            &data_path,
        )
        .expect("select");
        let contents = std::fs::read_to_string(&data_path).expect("read");
        assert_eq!(contents.trim(), "[]");
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
        select_file(
            &config_path,
            FILE_KEY,
            AdapterKindConfig::Json,
            None,
            None,
            &data_path,
        )
        .expect("select");

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
        let result = refresh_with_address(&config_path, &mut config, addr, &[FILE_KEY.to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn refresh_all_devices_handles_connection_errors() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config.json");
        let data_path = temp.path().join("data.json");
        init_config(&config_path, APP_ID_DEFAULT, None, None, true).expect("init");
        select_file(
            &config_path,
            FILE_KEY,
            AdapterKindConfig::Json,
            None,
            None,
            &data_path,
        )
        .expect("select");

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

        let changed =
            refresh_all_devices(&config_path, &mut config, false, &[FILE_KEY.to_string()])
                .expect("refresh");
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
    fn run_unlink_command() {
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
            command: Commands::Unlink {
                config: Some(config_path.clone()),
                device_id: "remote-device".to_string(),
                yes: true,
            },
        })
        .expect("run unlink");

        let config = load_config(&config_path).expect("load");
        assert!(!config.devices.contains_key("remote-device"));
    }
}
