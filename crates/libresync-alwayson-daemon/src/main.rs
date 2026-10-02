//! Headless managed multi-app companion. File secrets require --file-keys.
use libresync::{
    companion::{momentum_manifest, secure_companion_keys, CompanionManager},
    FileKeyStore, KeyStore, SessionInvitation,
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
fn run() -> libresync::Result<()> {
    let mut args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("libresync-alwayson-daemon [--root DIR] [--file-keys] [serve|status|add-momentum|enroll FILE|-|pause SPACE|resume SPACE|remove SPACE|backup SPACE|archive-legacy CONFIG]\nManaged app spaces use secure OS storage by default. --file-keys explicitly stores owner-only secret files for headless installations. Legacy config/data are never opened for network service automatically.");
        return Ok(());
    }
    let file_keys = if let Some(i) = args.iter().position(|s| s == "--file-keys") {
        args.remove(i);
        true
    } else {
        false
    };
    let root = if let Some(i) = args.iter().position(|s| s == "--root") {
        if i + 1 >= args.len() {
            return Err(libresync::Error::Protocol("--root needs directory".into()));
        }
        let p = PathBuf::from(args.remove(i + 1));
        args.remove(i);
        p
    } else {
        let dirs = directories::ProjectDirs::from("com", "codedbydan", "LibreSyncAlwaysOn")
            .ok_or_else(|| libresync::Error::Protocol("no data directory".into()))?;
        dirs.data_dir().join("managed-v1")
    };
    let keys: Arc<dyn KeyStore> = if file_keys {
        Arc::new(FileKeyStore::new(root.join("explicit-file-keys"))?)
    } else {
        secure_companion_keys()?
    };
    let mut manager = CompanionManager::open(root, &whoami::devicename(), keys, true)?;
    let command = args.first().map(String::as_str).unwrap_or("serve");
    let argument = || {
        args.get(1)
            .ok_or_else(|| libresync::Error::Protocol("command needs app space or file".into()))
    };
    match command {
        "serve" => {
            let running = Arc::new(AtomicBool::new(true));
            let signal = running.clone();
            ctrlc::set_handler(move || signal.store(false, Ordering::SeqCst))
                .map_err(|e| libresync::Error::Protocol(e.to_string()))?;
            while running.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        "status" => println!("{}", serde_json::to_string_pretty(&manager.snapshots()?)?),
        "add-momentum" => println!("{}", manager.add_app(momentum_manifest())?),
        "enroll" => {
            let source = argument()?;
            let data = if source == "-" {
                use std::io::Read;
                let mut text = String::new();
                std::io::stdin()
                    .take(16 * 1024 + 1)
                    .read_to_string(&mut text)?;
                text
            } else {
                std::fs::read_to_string(source)?
            };
            let invite = SessionInvitation::decode(&data)?;
            println!("{}", manager.enroll(&invite)?);
        }
        "pause" => manager.pause(argument()?)?,
        "resume" => manager.resume(argument()?)?,
        "remove" => manager.remove(argument()?)?,
        "backup" => println!("{}", manager.backup(argument()?)?),
        "archive-legacy" => println!(
            "{}",
            manager
                .archive_legacy(std::path::Path::new(argument()?))?
                .display()
        ),
        _ => {
            return Err(libresync::Error::Protocol(
                "unknown command; use --help".into(),
            ))
        }
    }
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1)
    }
}
