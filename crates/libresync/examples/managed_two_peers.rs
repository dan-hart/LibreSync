//! cargo run -p libresync --example managed_two_peers
use libresync::{
    AdapterDescriptor, AppManifest, DeviceMetadata, FileKeyStore, Session, SessionConfig,
};
use std::{
    fs,
    io::Write,
    path::Path,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
fn open(path: &Path, name: &str) -> libresync::Result<Session> {
    let metadata = DeviceMetadata {
        display_name: name.into(),
        device_kind: "desktop".into(),
        role: "application".into(),
        manifest: AppManifest {
            app_id: "io.libresync.managed-demo".into(),
            display_name: "Managed demo".into(),
            schema_version: 1,
            adapters: vec![AdapterDescriptor {
                id: "notes".into(),
                namespace: "io.libresync.managed-demo".into(),
                schema: "notes-v1".into(),
                transactional: true,
            }],
        },
    };
    Session::open(
        SessionConfig::new(path.join("session"), metadata),
        Arc::new(FileKeyStore::new(path.join("development-keys"))?),
    )
}
fn wait(mut predicate: impl FnMut() -> libresync::Result<bool>) -> libresync::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !predicate()? {
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "managed demo did not converge",
            )
            .into());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}
fn main() -> libresync::Result<()> {
    let root = tempfile::tempdir()?;
    let a = open(&root.path().join("a"), "Laptop")?;
    let b = open(&root.path().join("b"), "Desktop")?;
    a.start()?;
    b.start()?;
    a.set("notes", "greeting", b"Hello from the laptop".to_vec())?;
    b.connect(&a.create_invitation(Duration::from_secs(60))?)?;
    wait(|| Ok(b.get("notes", "greeting")?.is_some()))?;
    println!("The desktop durably stored the first note; application processing is still pending.");
    let inbox = b.application_inbox()?;
    // The app owns this separate model. Commit it durably BEFORE acknowledging
    // the exact inbox captured above; Session never replaces arbitrary files.
    let app = root.path().join("desktop-application.json");
    let temp = app.with_extension("tmp");
    let mut file = fs::File::create(&temp)?;
    file.write_all(&serde_json::to_vec(&inbox.records)?)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temp, app)?;
    fs::File::open(root.path())?.sync_all()?;
    b.acknowledge_inbox(&inbox)?;
    wait(|| {
        Ok(a.snapshot()?
            .peers
            .iter()
            .any(|p| !p.applied.epoch.is_empty() && p.applied.sequence > 0))
    })?;
    println!("The laptop now has a receipt for the desktop's committed application model.");
    b.pause()?;
    a.set(
        "notes",
        "greeting",
        b"Edited while the desktop was paused".to_vec(),
    )?;
    b.resume()?;
    wait(|| {
        Ok(b.get("notes", "greeting")? == Some(b"Edited while the desktop was paused".to_vec()))
    })?;
    println!("Resume caught up automatically, with the same enrollment.");
    a.shutdown()?;
    b.shutdown()?;
    Ok(())
}
