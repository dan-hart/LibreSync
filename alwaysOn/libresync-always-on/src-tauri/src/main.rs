#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod launch;
use libresync::{
    companion::{invitation_qr, secure_companion_keys, CompanionManager, SpaceSnapshot},
    BootstrapDecision, SessionInvitation,
};
use serde::Serialize;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::{Manager, State};
type Managed = Arc<Mutex<CompanionManager>>;
struct Runtime {
    manager: Managed,
    legacy: std::path::PathBuf,
    development_mode: bool,
}
type Reply<T> = Result<T, String>;
async fn work<T: Send + 'static>(job: impl FnOnce() -> Reply<T> + Send + 'static) -> Reply<T> {
    tauri::async_runtime::spawn_blocking(job)
        .await
        .map_err(|_| "Background worker unavailable")?
}
fn session(manager: &Managed, id: &str) -> Reply<Arc<libresync::Session>> {
    manager
        .lock()
        .map_err(|_| "Manager unavailable")?
        .session(id)
        .map_err(|e| e.to_string())
}
#[tauri::command]
async fn dashboard(runtime: State<'_, Runtime>) -> Reply<serde_json::Value> {
    let manager = runtime.manager.clone();
    let legacy = libresync::companion::legacy_config_available(&runtime.legacy);
    let development = runtime.development_mode;
    work(move||{let manager=manager.lock().map_err(|_|"Manager unavailable")?;let spaces:Vec<SpaceSnapshot>=manager.snapshots().map_err(|e|e.to_string())?;Ok(serde_json::json!({"spaces":spaces,"legacy_available":legacy,"supported_apps":manager.supported_apps(),"local_only":true,"development_mode":development}))}).await
}
#[tauri::command]
async fn add_app(runtime: State<'_, Runtime>, choice: usize) -> Reply<String> {
    let manager = runtime.manager.clone();
    work(move || {
        let mut manager = manager.lock().map_err(|_| "Manager unavailable")?;
        let manifest = manager
            .supported_apps()
            .get(choice)
            .cloned()
            .ok_or("Supported app absent")?;
        manager.add_app(manifest).map_err(|e| e.to_string())
    })
    .await
}
#[tauri::command]
async fn connect_invitation(
    runtime: State<'_, Runtime>,
    space: Option<String>,
    encoded: String,
    repair_peer: Option<String>,
) -> Reply<String> {
    let manager = runtime.manager.clone();
    work(move || {
        let invitation = decode_for_dashboard(&encoded)?;
        let id = match space {
            Some(id) => id,
            None => manager
                .lock()
                .map_err(|_| "Manager unavailable")?
                .add_app(invitation.invitation.metadata.manifest.clone())
                .map_err(|e| e.to_string())?,
        };
        let session = session(&manager, &id)?;
        match repair_peer {
            Some(peer) => session.repair_peer(&peer, &invitation),
            None => session.connect(&invitation),
        }
        .map_err(|e| e.to_string())?;
        Ok(id)
    })
    .await
}
#[derive(Serialize)]
struct InvitationView {
    encoded: String,
    svg: String,
    code: Option<String>,
    expires_at: u64,
}
#[tauri::command]
async fn invitation(
    runtime: State<'_, Runtime>,
    space: String,
    code: bool,
) -> Reply<InvitationView> {
    let manager = runtime.manager.clone();
    work(move || {
        let session = session(&manager, &space)?;
        let invite = if code {
            session.create_code_invitation(Duration::from_secs(300))
        } else {
            session.create_invitation(Duration::from_secs(300))
        }
        .map_err(|e| e.to_string())?;
        Ok(InvitationView {
            encoded: invite.encode().map_err(|e| e.to_string())?,
            svg: invitation_qr(&invite).map_err(|e| e.to_string())?,
            code: code.then(|| invite.invitation.secret.clone()),
            expires_at: invite.invitation.expires_at,
        })
    })
    .await
}
#[tauri::command]
async fn close_invitation(runtime: State<'_, Runtime>, space: String) -> Reply<()> {
    let manager = runtime.manager.clone();
    work(move || {
        session(&manager, &space)?
            .close_pairing()
            .map_err(|e| e.to_string())
    })
    .await
}
#[tauri::command]
async fn discover(runtime: State<'_, Runtime>, space: String) -> Reply<serde_json::Value> {
    let manager = runtime.manager.clone();
    work(move || {
        manager
            .lock()
            .map_err(|_| "Manager unavailable")?
            .nearby_devices(&space)
            .map_err(|e| e.to_string())
    })
    .await
}
#[tauri::command]
async fn connect_code(
    runtime: State<'_, Runtime>,
    space: String,
    device: String,
    code: String,
) -> Reply<()> {
    let manager = runtime.manager.clone();
    work(move || {
        let session = session(&manager, &space)?;
        let peer = session
            .discovered_peers()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|p| p.identity.device_id == device)
            .ok_or(
                "Device is no longer available. Keep both apps open on the same local network.",
            )?;
        if peer
            .invitation
            .as_ref()
            .is_some_and(|i| i.version != libresync::PAIRING_VERSION)
        {
            return Err(
                "This device uses an older pairing protocol. Upgrade its app before connecting."
                    .into(),
            );
        }
        session
            .connect_code(&peer, &code)
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
    .await
}
#[tauri::command]
async fn space_action(runtime: State<'_, Runtime>, space: String, action: String) -> Reply<()> {
    let manager = runtime.manager.clone();
    work(move || {
        let mut manager = manager.lock().map_err(|_| "Manager unavailable")?;
        match action.as_str() {
            "pause" => manager.pause(&space),
            "resume" => manager.resume(&space),
            "remove" => manager.remove(&space),
            "wake" => manager.session(&space).and_then(|s| s.wake()),
            _ => return Err("Unknown action".into()),
        }
        .map_err(|e| e.to_string())
    })
    .await
}
#[tauri::command]
async fn peer_action(
    runtime: State<'_, Runtime>,
    space: String,
    device: String,
    action: String,
) -> Reply<()> {
    let manager = runtime.manager.clone();
    work(move || {
        let session = session(&manager, &space)?;
        match action.as_str() {
            "pause" => session.pause_peer(&device),
            "resume" => session.resume_peer(&device),
            "remove" => session.remove_peer(&device),
            _ => return Err("Unknown device action".into()),
        }
        .map_err(|e| e.to_string())
    })
    .await
}
#[tauri::command]
async fn merge_preview(
    runtime: State<'_, Runtime>,
    space: String,
    device: String,
) -> Reply<serde_json::Value> {
    let manager = runtime.manager.clone();
    work(move||{let preview=session(&manager,&space)?.bootstrap_preview(&device).map_err(|e|e.to_string())?;Ok(match preview {Some(p)=>serde_json::json!({"token":p.token,"peer":p.peer,"records":p.batch.records.len(),"deletions":p.batch.records.iter().filter(|r|r.deleted).count(),"local_revision":p.local_revision}),None=>serde_json::Value::Null})}).await
}
#[tauri::command]
async fn merge_decision(
    runtime: State<'_, Runtime>,
    space: String,
    device: String,
    token: String,
    merge: bool,
) -> Reply<()> {
    let manager = runtime.manager.clone();
    work(move || {
        let session = session(&manager, &space)?;
        let preview = session
            .bootstrap_preview(&device)
            .map_err(|e| e.to_string())?
            .ok_or("Merge request no longer pending")?;
        if preview.token != token {
            return Err("Data changed. Review the new merge preview.".into());
        }
        session
            .resolve_bootstrap(
                &preview,
                if merge {
                    BootstrapDecision::Merge
                } else {
                    BootstrapDecision::Cancel
                },
            )
            .map_err(|e| e.to_string())
    })
    .await
}
#[tauri::command]
async fn backup(runtime: State<'_, Runtime>, space: String) -> Reply<String> {
    let manager = runtime.manager.clone();
    work(move || {
        manager
            .lock()
            .map_err(|_| "Manager unavailable")?
            .backup(&space)
            .map_err(|e| e.to_string())
    })
    .await
}
#[tauri::command]
async fn export_backup(
    runtime: State<'_, Runtime>,
    space: String,
    name: String,
    confirmed: bool,
) -> Reply<String> {
    if !confirmed {
        return Err("Confirm export of unencrypted recovery data first.".into());
    }
    let manager = runtime.manager.clone();
    work(move || {
        serde_json::to_string_pretty(
            &manager
                .lock()
                .map_err(|_| "Manager unavailable")?
                .export_backup(&space, &name)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    })
    .await
}
#[tauri::command]
async fn export_recovery(
    runtime: State<'_, Runtime>,
    space: String,
    snapshot: String,
    confirmed: bool,
) -> Reply<String> {
    if !confirmed {
        return Err("Confirm export of private unencrypted pre-merge data first.".into());
    }
    let manager = runtime.manager.clone();
    work(move || {
        serde_json::to_string_pretty(
            &manager
                .lock()
                .map_err(|_| "Manager unavailable")?
                .export_recovery(&space, &snapshot)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    })
    .await
}
#[tauri::command]
async fn archive_legacy(runtime: State<'_, Runtime>, confirmed: bool) -> Reply<String> {
    if !confirmed {
        return Err("Confirm a recovery copy of the old configuration first.".into());
    }
    let manager = runtime.manager.clone();
    let legacy = runtime.legacy.clone();
    work(move || {
        Ok(manager
            .lock()
            .map_err(|_| "Manager unavailable")?
            .archive_legacy(&legacy)
            .map_err(|e| e.to_string())?
            .display()
            .to_string())
    })
    .await
}
fn show_dashboard(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        // Reuse the hidden webview and its existing managed runtime/lease.
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}
fn main() {
    let result = tauri::Builder::default()
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(|app| {
            let args = std::env::args().collect::<Vec<_>>();
            let environment = [
                "LIBRESYNC_DEVELOPMENT_ROOT",
                "LIBRESYNC_DEVELOPMENT_FILE_KEYS",
                "LIBRESYNC_DEVELOPMENT_DEVICE_NAME",
            ]
            .into_iter()
            .filter_map(|name| {
                std::env::var_os(name)
                    .map(|value| (name.to_owned(), value.to_string_lossy().into_owned()))
            })
            .collect::<Vec<_>>();
            let options =
                launch::LaunchOptions::parse(&args, &environment, cfg!(debug_assertions))?;
            let development_root = options.development_root;
            let file_keys = options.file_keys;
            let base = development_root
                .as_ref()
                .map(std::path::PathBuf::from)
                .unwrap_or(app.path().app_data_dir()?);
            let keys: Arc<dyn libresync::KeyStore> = if file_keys {
                Arc::new(libresync::FileKeyStore::new(
                    base.join("development-file-keys"),
                )?)
            } else {
                secure_companion_keys()?
            };
            let device_name = options.device_name.unwrap_or_else(whoami::devicename);
            let manager =
                CompanionManager::open(base.join("managed-v1"), &device_name, keys, true)?;
            app.manage(Runtime {
                manager: Arc::new(Mutex::new(manager)),
                legacy: if development_root.is_some() {
                    base.join("config.json")
                } else {
                    app.path()
                        .data_dir()?
                        .join("com.tauri.dev")
                        .join("config.json")
                },
                development_mode: development_root.is_some(),
            });
            let show =
                tauri::menu::MenuItem::with_id(app, "show", "Open dashboard", true, None::<&str>)?;
            let quit = tauri::menu::MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = tauri::menu::Menu::with_items(app, &[&show, &quit])?;
            let mut tray = tauri::tray::TrayIconBuilder::new()
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_dashboard(app),
                    "quit" => {
                        let manager = app.state::<Runtime>().manager.clone();
                        let handle = app.clone();
                        tauri::async_runtime::spawn_blocking(move || {
                            if let Ok(manager) = manager.lock() {
                                let _ = manager.shutdown();
                            }
                            handle.exit(0);
                        });
                    }
                    _ => {}
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            dashboard,
            add_app,
            connect_invitation,
            invitation,
            close_invitation,
            discover,
            connect_code,
            space_action,
            peer_action,
            merge_preview,
            merge_decision,
            backup,
            export_backup,
            export_recovery,
            archive_legacy
        ])
        .build(tauri::generate_context!());
    match result {
        Ok(app) => app.run(|handle, event| {
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                show_dashboard(handle);
            }
            #[cfg(not(target_os = "macos"))]
            let _ = (handle, event);
        }),
        Err(error) => {
            eprintln!("AlwaysOn could not start: {error}");
            std::process::exit(1)
        }
    }
}

fn decode_for_dashboard(encoded: &str) -> Reply<SessionInvitation> {
    if encoded.len() > 16 * 1024 {
        return Err("Invitation too large. Open a new invitation in the app.".into());
    }
    // Public version is only an untrusted UX hint. Strict decoding and the
    // authenticated pairing handshake still validate every accepted invitation.
    let public: serde_json::Value = serde_json::from_str(encoded).map_err(|e| e.to_string())?;
    if public
        .get("invitation")
        .and_then(|v| v.get("version"))
        .and_then(|v| v.as_u64())
        .is_some_and(|version| version != u64::from(libresync::PAIRING_VERSION))
    {
        return Err("This invitation needs a compatible app upgrade. Update both apps and open a new invitation.".into());
    }
    SessionInvitation::decode(encoded).map_err(|e| e.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_pasted_legacy_invitation_has_upgrade_guidance() {
        let root =
            std::env::temp_dir().join(format!("libresync-paste-test-{}", uuid::Uuid::new_v4()));
        let mut config = libresync::SessionConfig::new(
            &root,
            libresync::DeviceMetadata {
                display_name: "Paste regression".into(),
                device_kind: "computer".into(),
                role: "application".into(),
                manifest: libresync::companion::notes_manifest(),
            },
        );
        config.advertise = false;
        let app =
            libresync::Session::open(config, Arc::new(libresync::MemoryKeyStore::new())).unwrap();
        app.start().unwrap();
        let mut invitation = app.create_invitation(Duration::from_secs(60)).unwrap();
        let current = invitation.encode().unwrap();
        assert!(decode_for_dashboard(&current).is_ok());
        invitation.invitation.version = 1;
        let legacy = invitation.encode().unwrap();
        assert!(SessionInvitation::decode(&legacy).is_err());
        let error = decode_for_dashboard(&legacy).unwrap_err();
        app.shutdown().unwrap();
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
        assert!(error.contains("upgrade"), "{error}");
        assert!(!decode_for_dashboard(&" ".repeat(16385))
            .unwrap_err()
            .contains("upgrade"));
        assert!(!decode_for_dashboard(r#"{"invitation":{"version":2}}"#)
            .unwrap_err()
            .contains("upgrade"));
    }
}
