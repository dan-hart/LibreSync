//! Local notes sample for connecting to the AlwaysOn dashboard. All state is
//! isolated under the supplied example directory; file keys are explicit here.
use libresync::{companion::notes_manifest, DeviceMetadata, FileKeyStore, Session, SessionConfig};
use std::{io, path::PathBuf, sync::Arc, time::Duration};
fn main() -> libresync::Result<()> {
    let root = std::env::args().nth(1).map(PathBuf::from).ok_or_else(|| {
        libresync::Error::Protocol("usage: companion_app EXAMPLE_DIRECTORY [--code]".into())
    })?;
    let config = SessionConfig::new(
        root.join("session"),
        DeviceMetadata {
            display_name: "Local notes sample laptop".into(),
            device_kind: "desktop".into(),
            role: "application".into(),
            manifest: notes_manifest(),
        },
    );
    let session = Session::open(
        config,
        Arc::new(FileKeyStore::new(root.join("explicit-example-keys"))?),
    )?;
    session.start()?;
    session.set(
        "records",
        "welcome",
        b"Hello from the local notes sample".to_vec(),
    )?;
    let code = std::env::args().any(|arg| arg == "--code");
    let invitation = if code {
        session.create_code_invitation(Duration::from_secs(300))?
    } else {
        session.create_invitation(Duration::from_secs(300))?
    };
    println!(
        "Local notes sample — explicit example file keys. Invitation expires after five minutes."
    );
    if code {
        println!("Code: {}", invitation.invitation.secret);
    } else {
        println!(
            "Paste this invitation into AlwaysOn:\n{}",
            invitation.encode()?
        );
    }
    println!("Keep this app open. Press Enter to stop. The sample stores records and does not pretend an app Applied transaction occurred.");
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    session.shutdown()
}
