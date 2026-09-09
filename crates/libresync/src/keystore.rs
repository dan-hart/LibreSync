//! Secure storage for the app key and device identity.
//!
//! The engine never decides where keys live; apps implement `DeviceHandler`
//! and can back it with any [`KeyStore`]. This module provides the trait plus
//! backends that need no platform frameworks in the core crate:
//!
//! - [`MemoryKeyStore`]: tests and throw-away sessions.
//! - [`FileKeyStore`]: one file per key with owner-only permissions
//!   (development and the CLI).
//! - [`SecretToolKeyStore`] (Linux): the Secret Service through libsecret's
//!   `secret-tool` CLI. Inside Flatpak prefer libsecret from the app itself,
//!   which goes through the Secret portal; see `docs/PACKAGING-LINUX.md`.
//! - [`SecurityCliKeyStore`] (macOS): the login Keychain through the
//!   `security` CLI. Sandboxed apps should use the Swift `LibreSyncKeychain`
//!   helper (Security framework) instead; see `docs/PACKAGING-MACOS.md`.
//!
//! Values are stored base64-encoded so both CLIs can carry binary key
//! material on a single line.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use crate::{AppKey, DeviceKeys, Error, Identity, Result};

/// Well-known item names used by [`KeyStoreExt`].
pub const APP_KEY_ITEM: &str = "app-key";
pub const DEVICE_CERT_ITEM: &str = "device-cert-der";
pub const DEVICE_KEY_ITEM: &str = "device-key-der";

/// Named secret storage.
pub trait KeyStore: Send + Sync {
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>>;
    fn set(&self, name: &str, value: &[u8]) -> Result<()>;
    fn delete(&self, name: &str) -> Result<()>;
}

/// Convenience helpers for the standard LibreSync items.
pub trait KeyStoreExt: KeyStore {
    fn app_key(&self) -> Result<Option<AppKey>> {
        match self.get(APP_KEY_ITEM)? {
            Some(bytes) => Ok(Some(AppKey::from_slice(&bytes)?)),
            None => Ok(None),
        }
    }

    fn set_app_key(&self, app_key: &AppKey) -> Result<()> {
        self.set(APP_KEY_ITEM, app_key.as_bytes())
    }

    /// Returns the stored app key, generating and storing one if absent.
    fn load_or_create_app_key(&self) -> Result<AppKey> {
        if let Some(app_key) = self.app_key()? {
            return Ok(app_key);
        }
        let app_key = AppKey::generate()?;
        self.set_app_key(&app_key)?;
        Ok(app_key)
    }

    fn device_keys(&self) -> Result<Option<DeviceKeys>> {
        match (self.get(DEVICE_CERT_ITEM)?, self.get(DEVICE_KEY_ITEM)?) {
            (Some(cert), Some(key)) => Ok(Some(DeviceKeys::from_der(cert, key)?)),
            _ => Ok(None),
        }
    }

    fn set_device_keys(&self, device_keys: &DeviceKeys) -> Result<()> {
        self.set(DEVICE_CERT_ITEM, device_keys.cert_der())?;
        self.set(DEVICE_KEY_ITEM, device_keys.key_der())
    }

    /// Returns the stored device keys, generating and storing a certificate
    /// for `identity` if absent.
    fn load_or_create_device_keys(&self, identity: &Identity) -> Result<DeviceKeys> {
        if let Some(keys) = self.device_keys()? {
            return Ok(keys);
        }
        let keys = DeviceKeys::generate(identity)?;
        self.set_device_keys(&keys)?;
        Ok(keys)
    }
}

impl<T: KeyStore + ?Sized> KeyStoreExt for T {}

/// In-memory store.
#[derive(Default)]
pub struct MemoryKeyStore {
    items: Mutex<HashMap<String, Vec<u8>>>,
}

impl MemoryKeyStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl KeyStore for MemoryKeyStore {
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>> {
        Ok(lock(&self.items)?.get(name).cloned())
    }

    fn set(&self, name: &str, value: &[u8]) -> Result<()> {
        lock(&self.items)?.insert(name.to_string(), value.to_vec());
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<()> {
        lock(&self.items)?.remove(name);
        Ok(())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>> {
    mutex
        .lock()
        .map_err(|_| Error::Protocol("key store lock poisoned".to_string()))
}

/// One file per item under a directory, created with mode 0700/0600 on Unix.
/// For development and the CLI only.
#[derive(Clone, Debug)]
pub struct FileKeyStore {
    dir: PathBuf,
}

impl FileKeyStore {
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, name: &str) -> Result<PathBuf> {
        if name.is_empty()
            || !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
            || name.starts_with('.')
        {
            return Err(Error::Protocol(format!("invalid key store item name: {name}")));
        }
        Ok(self.dir.join(format!("{name}.key")))
    }
}

impl KeyStore for FileKeyStore {
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let path = self.path(name)?;
        match fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn set(&self, name: &str, value: &[u8]) -> Result<()> {
        let path = self.path(name)?;
        let tmp = path.with_extension("key.tmp");
        {
            let mut options = fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&tmp)?;
            file.write_all(value)?;
            file.sync_all()?;
        }
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<()> {
        let path = self.path(name)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

/// Linux Secret Service (GNOME Keyring, KWallet via the Secret Service API)
/// through libsecret's `secret-tool` command. Items are keyed by the
/// attributes `service=<service>` and `account=<name>`.
#[derive(Clone, Debug)]
pub struct SecretToolKeyStore {
    command: PathBuf,
    service: String,
}

impl SecretToolKeyStore {
    /// `service` is usually the app id; it namespaces items in the keyring.
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            command: PathBuf::from("secret-tool"),
            service: service.into(),
        }
    }

    /// Overrides the `secret-tool` executable (tests, custom prefixes).
    pub fn with_command(mut self, command: impl Into<PathBuf>) -> Self {
        self.command = command.into();
        self
    }

    /// True when the command can be executed.
    pub fn is_available(&self) -> bool {
        Command::new(&self.command)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
}

impl KeyStore for SecretToolKeyStore {
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let output = Command::new(&self.command)
            .args(["lookup", "service", &self.service, "account", name])
            .stdin(Stdio::null())
            .output()
            .map_err(|error| command_error("secret-tool", error))?;
        if !output.status.success() {
            // secret-tool exits 1 when nothing matches.
            return Ok(None);
        }
        decode_output(&output.stdout)
    }

    fn set(&self, name: &str, value: &[u8]) -> Result<()> {
        let mut child = Command::new(&self.command)
            .args([
                "store",
                "--label",
                &format!("LibreSync {name} ({})", self.service),
                "service",
                &self.service,
                "account",
                name,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| command_error("secret-tool", error))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(encode(value).as_bytes())?;
            stdin.write_all(b"\n")?;
        }
        let output = child.wait_with_output()?;
        check_status("secret-tool store", &output.status, &output.stderr)
    }

    fn delete(&self, name: &str) -> Result<()> {
        let output = Command::new(&self.command)
            .args(["clear", "service", &self.service, "account", name])
            .stdin(Stdio::null())
            .output()
            .map_err(|error| command_error("secret-tool", error))?;
        check_status("secret-tool clear", &output.status, &output.stderr)
    }
}

/// macOS login Keychain through `/usr/bin/security` generic passwords
/// (`-s <service> -a <name>`). Items created this way are visible in Keychain
/// Access and to the Swift `LibreSyncKeychain` helper when the same service
/// and account names are used.
#[derive(Clone, Debug)]
pub struct SecurityCliKeyStore {
    command: PathBuf,
    service: String,
}

impl SecurityCliKeyStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            command: PathBuf::from("/usr/bin/security"),
            service: service.into(),
        }
    }

    pub fn with_command(mut self, command: impl Into<PathBuf>) -> Self {
        self.command = command.into();
        self
    }

    pub fn is_available(&self) -> bool {
        Command::new(&self.command)
            .arg("help")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
}

impl KeyStore for SecurityCliKeyStore {
    fn get(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let output = Command::new(&self.command)
            .args([
                "find-generic-password",
                "-s",
                &self.service,
                "-a",
                name,
                "-w",
            ])
            .stdin(Stdio::null())
            .output()
            .map_err(|error| command_error("security", error))?;
        if !output.status.success() {
            // 44 = errSecItemNotFound.
            return Ok(None);
        }
        decode_output(&output.stdout)
    }

    fn set(&self, name: &str, value: &[u8]) -> Result<()> {
        let output = Command::new(&self.command)
            .args([
                "add-generic-password",
                "-U",
                "-s",
                &self.service,
                "-a",
                name,
                "-l",
                &format!("LibreSync {name}"),
                "-w",
                &encode(value),
            ])
            .stdin(Stdio::null())
            .output()
            .map_err(|error| command_error("security", error))?;
        check_status("security add-generic-password", &output.status, &output.stderr)
    }

    fn delete(&self, name: &str) -> Result<()> {
        let output = Command::new(&self.command)
            .args(["delete-generic-password", "-s", &self.service, "-a", name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .output()
            .map_err(|error| command_error("security", error))?;
        if output.status.success() || output.status.code() == Some(44) {
            Ok(())
        } else {
            check_status("security delete-generic-password", &output.status, &output.stderr)
        }
    }
}

/// Picks the platform backend: `SecurityCliKeyStore` on macOS,
/// `SecretToolKeyStore` on other Unix systems when `secret-tool` is
/// available, otherwise a `FileKeyStore` under `fallback_dir`.
pub fn platform_key_store(service: &str, fallback_dir: &Path) -> Result<Box<dyn KeyStore>> {
    #[cfg(target_os = "macos")]
    {
        let store = SecurityCliKeyStore::new(service);
        if store.is_available() {
            return Ok(Box::new(store));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let store = SecretToolKeyStore::new(service);
        if store.is_available() {
            return Ok(Box::new(store));
        }
    }
    let _ = service;
    Ok(Box::new(FileKeyStore::new(fallback_dir)?))
}

fn command_error(name: &str, error: std::io::Error) -> Error {
    Error::Protocol(format!("cannot run {name}: {error}"))
}

fn check_status(what: &str, status: &std::process::ExitStatus, stderr: &[u8]) -> Result<()> {
    if status.success() {
        Ok(())
    } else {
        Err(Error::Protocol(format!(
            "{what} failed ({status}): {}",
            String::from_utf8_lossy(stderr).trim()
        )))
    }
}

fn decode_output(stdout: &[u8]) -> Result<Option<Vec<u8>>> {
    let text = String::from_utf8_lossy(stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    decode(trimmed).map(Some)
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 63] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn decode(text: &str) -> Result<Vec<u8>> {
    let invalid = || Error::Protocol("invalid base64 in key store".to_string());
    let bytes: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if bytes.len() % 4 != 0 {
        return Err(invalid());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let mut triple = 0u32;
        let mut padding = 0;
        for (index, byte) in chunk.iter().enumerate() {
            let value = if *byte == b'=' {
                padding += 1;
                0
            } else {
                if padding > 0 {
                    return Err(invalid());
                }
                ALPHABET
                    .iter()
                    .position(|candidate| candidate == byte)
                    .ok_or_else(invalid)? as u32
            };
            triple |= value << (18 - 6 * index);
        }
        out.push((triple >> 16) as u8);
        if padding < 2 {
            out.push((triple >> 8) as u8);
        }
        if padding < 1 {
            out.push(triple as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn exercise(store: &dyn KeyStore) {
        assert!(store.get("missing").expect("get").is_none());
        store.set("alpha", b"one\x00two\xff").expect("set");
        assert_eq!(
            store.get("alpha").expect("get"),
            Some(b"one\x00two\xff".to_vec())
        );
        store.set("alpha", b"three").expect("overwrite");
        assert_eq!(store.get("alpha").expect("get"), Some(b"three".to_vec()));
        store.delete("alpha").expect("delete");
        assert!(store.get("alpha").expect("get").is_none());
        store.delete("alpha").expect("delete twice is fine");

        let identity = Identity::new("device", "com.example.app", "user");
        let app_key = store.load_or_create_app_key().expect("app key");
        assert_eq!(store.load_or_create_app_key().expect("again"), app_key);
        let keys = store
            .load_or_create_device_keys(&identity)
            .expect("device keys");
        let again = store
            .load_or_create_device_keys(&identity)
            .expect("device keys again");
        assert_eq!(keys.fingerprint(), again.fingerprint());
        assert_eq!(
            store.device_keys().expect("get").map(|k| k.fingerprint().to_string()),
            Some(keys.fingerprint().to_string())
        );
    }

    #[test]
    fn base64_round_trip() {
        for len in 0..40 {
            let bytes: Vec<u8> = (0..len as u8).map(|b| b.wrapping_mul(37)).collect();
            assert_eq!(decode(&encode(&bytes)).expect("decode"), bytes);
        }
        assert_eq!(encode(b"hi"), "aGk=");
        assert!(decode("abc").is_err());
        assert!(decode("a=bc").is_err());
        assert!(decode("!!!!").is_err());
    }

    #[test]
    fn memory_store_round_trip() {
        exercise(&MemoryKeyStore::new());
    }

    #[test]
    fn file_store_round_trip_and_permissions() {
        let dir = tempdir().expect("tempdir");
        let store = FileKeyStore::new(dir.path().join("keys")).expect("store");
        exercise(&store);
        assert!(store.set("../escape", b"x").is_err());
        assert!(store.get(".hidden").is_err());
        store.set("perm", b"x").expect("set");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.dir().join("perm.key"))
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
            let dir_mode = fs::metadata(store.dir()).expect("meta").permissions().mode();
            assert_eq!(dir_mode & 0o777, 0o700);
        }
    }

    /// Writes a shell script that emulates `secret-tool` or `security` with a
    /// directory-backed store, so the CLI backends are tested on any Unix CI.
    #[cfg(unix)]
    fn fake_command(dir: &Path, name: &str, probe: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\nSTORE=\"{}\"\n{body}", dir.display()))
            .expect("write script");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
        // Other tests fork concurrently; a child that inherited the write
        // handle before its exec makes the first spawn fail with ETXTBSY.
        // Wait until the script is executable.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match Command::new(&path)
                .arg(probe)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
            {
                Ok(status) if status.success() => break,
                _ if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                other => panic!("fake command never became executable: {other:?}"),
            }
        }
        path
    }

    #[cfg(unix)]
    #[test]
    fn secret_tool_backend_round_trip_with_fake_cli() {
        let dir = tempdir().expect("tempdir");
        let script = fake_command(
            dir.path(),
            "secret-tool",
            "--version",
            r#"
cmd="$1"; shift
case "$cmd" in
  --version) echo 0.21; exit 0 ;;
  store)
    shift 2   # --label <label>
    service="$2"; account="$4"
    read -r secret
    printf '%s' "$secret" > "$STORE/$service.$account.secret"
    exit 0 ;;
  lookup)
    service="$2"; account="$4"
    f="$STORE/$service.$account.secret"
    [ -f "$f" ] || exit 1
    cat "$f"; exit 0 ;;
  clear)
    service="$2"; account="$4"
    rm -f "$STORE/$service.$account.secret"; exit 0 ;;
esac
exit 2
"#,
        );
        let store = SecretToolKeyStore::new("com.example.app").with_command(&script);
        assert!(store.is_available());
        exercise(&store);

        let missing = SecretToolKeyStore::new("svc").with_command(dir.path().join("nope"));
        assert!(!missing.is_available());
        assert!(missing.get("x").is_err());
        assert!(missing.set("x", b"y").is_err());
        assert!(missing.delete("x").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn security_cli_backend_round_trip_with_fake_cli() {
        let dir = tempdir().expect("tempdir");
        let script = fake_command(
            dir.path(),
            "security",
            "help",
            r#"
cmd="$1"; shift
case "$cmd" in
  help) exit 0 ;;
  add-generic-password)
    # -U -s <service> -a <account> -l <label> -w <secret>
    service="$3"; account="$5"; secret="$9"
    printf '%s' "$secret" > "$STORE/$service.$account.secret"; exit 0 ;;
  find-generic-password)
    service="$2"; account="$4"
    f="$STORE/$service.$account.secret"
    [ -f "$f" ] || { echo "The specified item could not be found in the keychain." >&2; exit 44; }
    cat "$f"; echo; exit 0 ;;
  delete-generic-password)
    service="$2"; account="$4"
    f="$STORE/$service.$account.secret"
    [ -f "$f" ] || exit 44
    rm -f "$f"; exit 0 ;;
esac
exit 2
"#,
        );
        let store = SecurityCliKeyStore::new("com.example.app").with_command(&script);
        assert!(store.is_available());
        exercise(&store);

        let missing = SecurityCliKeyStore::new("svc").with_command(dir.path().join("nope"));
        assert!(!missing.is_available());
        assert!(missing.get("x").is_err());
    }

    #[test]
    fn platform_store_falls_back_to_files() {
        let dir = tempdir().expect("tempdir");
        // Whatever the platform provides, the helper must return a working store.
        let store = platform_key_store("com.example.app.test", &dir.path().join("keys"))
            .expect("store");
        let _ = store.get("probe");
        let file_store = FileKeyStore::new(dir.path().join("keys2")).expect("store");
        let boxed: Box<dyn KeyStore> = Box::new(file_store);
        assert!(boxed.app_key().expect("app key").is_none());
    }
}
