use std::io::BufReader;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, ServerConfig, ServerConnection,
    SignatureScheme, StreamOwned,
};

use crate::protocol::{read_message, write_message, Message};
use crate::{
    decrypt_entries, encrypt_entries, AppKey, DeviceHandler, DeviceKeys, Error, Identity, Result,
    State,
};

pub struct SyncListener {
    addr: SocketAddr,
    shutdown: mpsc::Sender<()>,
    handle: thread::JoinHandle<Result<()>>,
}

impl SyncListener {
    pub fn start(
        addr: SocketAddr,
        identity: Identity,
        state: Arc<Mutex<State>>,
        handler: Arc<dyn DeviceHandler>,
    ) -> Result<Self> {
        let listener = TcpListener::bind(addr)?;
        let local_addr = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let device_keys = handler.device_keys()?;

        let (shutdown_sender, shutdown_receiver) = mpsc::channel();
        let handle = thread::spawn(move || loop {
            if shutdown_receiver.try_recv().is_ok() {
                break Ok(());
            }

            match listener.accept() {
                Ok((stream, _)) => {
                    if let Err(error) =
                        handle_connection(stream, &identity, handler.as_ref(), &state, &device_keys)
                    {
                        #[cfg(test)]
                        eprintln!("sync listener error: {error}");
                        let _ = error;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => break Err(error.into()),
            }
        });

        Ok(Self {
            addr: local_addr,
            shutdown: shutdown_sender,
            handle,
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn shutdown(self) -> Result<()> {
        let _ = self.shutdown.send(());
        match self.handle.join() {
            Ok(result) => result,
            Err(_) => Err(Error::Protocol("sync listener panicked".to_string())),
        }
    }
}

pub fn sync_with_device<F>(
    identity: &Identity,
    state: &mut State,
    device: SocketAddr,
    device_keys: &DeviceKeys,
    app_key: &AppKey,
    device_check: F,
) -> Result<(Identity, String)>
where
    F: Fn(&Identity, &str) -> Result<()>,
{
    let (remote_identity, fingerprint) =
        push_snapshot(identity, state, device, device_keys, app_key, &device_check)?;
    pull_snapshot(
        identity,
        state,
        device,
        device_keys,
        app_key,
        &remote_identity,
        &fingerprint,
    )?;
    Ok((remote_identity, fingerprint))
}

pub fn link_with_device(
    identity: &Identity,
    device_keys: &DeviceKeys,
    app_key: &AppKey,
    device: SocketAddr,
    pairing_secret: Option<String>,
) -> Result<(Identity, String, AppKey)> {
    let stream = TcpStream::connect_timeout(&device, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    let stream = tls_client_stream(stream, device_keys)?;
    let mut reader = BufReader::new(stream);

    write_message(
        reader.get_mut(),
        &Message::LinkRequest {
            identity: identity.clone(),
            app_key: Some(app_key.as_bytes().to_vec()),
            pairing_secret,
        },
    )?;

    let response = read_message(&mut reader)?;
    let fingerprint = device_fingerprint(reader.get_mut().conn.peer_certificates())?;
    let (remote_identity, accepted, remote_app_key) = match response {
        Message::LinkResponse {
            identity,
            accepted,
            app_key,
        } => (identity, accepted, app_key),
        _ => return Err(Error::Protocol("unexpected linking response".to_string())),
    };

    if !accepted {
        return Err(Error::Protocol(format!(
            "linking rejected by {}",
            remote_identity.device_id
        )));
    }

    let key_bytes = remote_app_key
        .ok_or_else(|| Error::Protocol("linking response missing app key".to_string()))?;
    let remote_app_key = AppKey::from_slice(&key_bytes)?;

    if remote_identity.app_id != identity.app_id {
        return Err(Error::Protocol(
            "app id mismatch during linking".to_string(),
        ));
    }

    Ok((remote_identity, fingerprint, remote_app_key))
}

fn handle_connection(
    stream: TcpStream,
    identity: &Identity,
    handler: &dyn DeviceHandler,
    state: &Arc<Mutex<State>>,
    device_keys: &DeviceKeys,
) -> Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    let stream = tls_server_stream(stream, device_keys)?;
    let mut reader = BufReader::new(stream);

    let message = read_message(&mut reader)?;
    let fingerprint = device_fingerprint(reader.get_mut().conn.peer_certificates())?;
    let app_key = handler.app_key()?;

    match message {
        Message::LinkRequest {
            identity: device_identity,
            app_key: remote_app_key,
            pairing_secret,
        } => {
            if !device_identity.matches_app(handler.app_id()) {
                write_message(
                    reader.get_mut(),
                    &Message::LinkResponse {
                        identity: identity.clone(),
                        accepted: false,
                        app_key: None,
                    },
                )?;
                return Ok(());
            }
            if let Some(expected) = handler.pairing_secret() {
                let provided = pairing_secret.unwrap_or_default();
                if provided != expected {
                    write_message(
                        reader.get_mut(),
                        &Message::LinkResponse {
                            identity: identity.clone(),
                            accepted: false,
                            app_key: None,
                        },
                    )?;
                    return Ok(());
                }
            }
            let incoming_key = remote_app_key
                .ok_or_else(|| Error::Protocol("linking request missing app key".to_string()))?;
            let incoming_key = AppKey::from_slice(&incoming_key)?;
            let accepted = handler.approve_link_with_fingerprint(&device_identity, &fingerprint)?;
            if accepted && incoming_key != app_key {
                handler.set_app_key(&incoming_key)?;
            }
            let response_key = if accepted {
                Some(handler.app_key()?.as_bytes().to_vec())
            } else {
                None
            };
            write_message(
                reader.get_mut(),
                &Message::LinkResponse {
                    identity: identity.clone(),
                    accepted,
                    app_key: response_key,
                },
            )?;
        }
        Message::Hello {
            identity: device_identity,
        } => {
            if !device_identity.matches_app(handler.app_id()) {
                return Err(Error::Protocol("app id mismatch".to_string()));
            }
            if !handler.is_linked_with_fingerprint(&device_identity, &fingerprint) {
                return Err(Error::Protocol("device not linked".to_string()));
            }
            write_message(
                reader.get_mut(),
                &Message::Hello {
                    identity: identity.clone(),
                },
            )?;

            match read_message(&mut reader)? {
                Message::SnapshotRequest => {
                    let snapshot = {
                        let state = state.lock().expect("state poisoned");
                        encrypt_entries(&app_key, state.snapshot())?
                    };
                    write_message(reader.get_mut(), &Message::Snapshot { entries: snapshot })?;
                }
                Message::Snapshot { entries } => {
                    let entries = decrypt_entries(&app_key, entries)?;
                    let mut state = state.lock().expect("state poisoned");
                    state.merge_snapshot(entries);
                    write_message(reader.get_mut(), &Message::Ack)?;
                }
                _ => {
                    return Err(Error::Protocol(
                        "expected snapshot request or snapshot".to_string(),
                    ))
                }
            }
        }
        _ => {
            return Err(Error::Protocol(
                "expected hello or link request".to_string(),
            ))
        }
    }

    Ok(())
}

pub(crate) fn push_snapshot<F>(
    identity: &Identity,
    state: &State,
    device: SocketAddr,
    device_keys: &DeviceKeys,
    app_key: &AppKey,
    device_check: &F,
) -> Result<(Identity, String)>
where
    F: Fn(&Identity, &str) -> Result<()>,
{
    let stream = TcpStream::connect_timeout(&device, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    let stream = tls_client_stream(stream, device_keys)?;
    let mut reader = BufReader::new(stream);

    write_message(
        reader.get_mut(),
        &Message::Hello {
            identity: identity.clone(),
        },
    )?;
    let remote_identity = read_hello(&mut reader, identity)?;
    let fingerprint = device_fingerprint(reader.get_mut().conn.peer_certificates())?;
    device_check(&remote_identity, &fingerprint)?;

    let snapshot = encrypt_entries(app_key, state.snapshot())?;
    write_message(reader.get_mut(), &Message::Snapshot { entries: snapshot })?;

    let ack = read_message(&mut reader)?;
    if ack != Message::Ack {
        return Err(Error::Protocol("expected ack".to_string()));
    }

    Ok((remote_identity, fingerprint))
}

pub(crate) fn pull_snapshot(
    identity: &Identity,
    state: &mut State,
    device: SocketAddr,
    device_keys: &DeviceKeys,
    app_key: &AppKey,
    expected_remote: &Identity,
    expected_fingerprint: &str,
) -> Result<()> {
    let stream = TcpStream::connect_timeout(&device, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    let stream = tls_client_stream(stream, device_keys)?;
    let mut reader = BufReader::new(stream);

    write_message(
        reader.get_mut(),
        &Message::Hello {
            identity: identity.clone(),
        },
    )?;
    let remote_identity = read_hello(&mut reader, identity)?;
    if remote_identity != *expected_remote {
        return Err(Error::Protocol("device identity changed".to_string()));
    }
    let fingerprint = device_fingerprint(reader.get_mut().conn.peer_certificates())?;
    if fingerprint != expected_fingerprint {
        return Err(Error::Protocol("device fingerprint changed".to_string()));
    }

    write_message(reader.get_mut(), &Message::SnapshotRequest)?;

    let response = read_message(&mut reader)?;
    let entries = match response {
        Message::Snapshot { entries } => entries,
        _ => return Err(Error::Protocol("expected snapshot".to_string())),
    };
    let entries = decrypt_entries(app_key, entries)?;
    state.merge_snapshot(entries);

    Ok(())
}

fn read_hello<R: std::io::BufRead>(reader: &mut R, identity: &Identity) -> Result<Identity> {
    match read_message(reader)? {
        Message::Hello { identity: remote } => {
            if remote.app_id != identity.app_id {
                return Err(Error::Protocol("app id mismatch".to_string()));
            }
            Ok(remote)
        }
        _ => Err(Error::Protocol("expected hello".to_string())),
    }
}

fn tls_client_stream(
    stream: TcpStream,
    device_keys: &DeviceKeys,
) -> Result<StreamOwned<ClientConnection, TcpStream>> {
    let config = client_config(device_keys)?;
    let server_name = ServerName::try_from("libresync.local")
        .map_err(|_| Error::Protocol("invalid server name".to_string()))?;
    let conn = ClientConnection::new(config, server_name)?;
    Ok(StreamOwned::new(conn, stream))
}

fn tls_server_stream(
    stream: TcpStream,
    device_keys: &DeviceKeys,
) -> Result<StreamOwned<ServerConnection, TcpStream>> {
    let config = server_config(device_keys)?;
    let conn = ServerConnection::new(config)?;
    Ok(StreamOwned::new(conn, stream))
}

fn client_config(device_keys: &DeviceKeys) -> Result<Arc<ClientConfig>> {
    let certs = vec![CertificateDer::from(device_keys.cert_der().to_vec())];
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(device_keys.key_der().to_vec()));
    let verifier = Arc::new(AcceptAnyServerVerifier);

    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(certs, key)?;
    Ok(Arc::new(config))
}

fn server_config(device_keys: &DeviceKeys) -> Result<Arc<ServerConfig>> {
    let certs = vec![CertificateDer::from(device_keys.cert_der().to_vec())];
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(device_keys.key_der().to_vec()));
    let verifier = Arc::new(AcceptAnyClientVerifier);

    let config = ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs, key)?;
    Ok(Arc::new(config))
}

fn device_fingerprint(certs: Option<&[CertificateDer<'static>]>) -> Result<String> {
    let cert = certs
        .and_then(|certs| certs.first())
        .ok_or_else(|| Error::Protocol("missing device certificate".to_string()))?;
    Ok(crate::keys::fingerprint_cert(cert.as_ref()))
}

#[derive(Debug)]
struct AcceptAnyServerVerifier;

impl ServerCertVerifier for AcceptAnyServerVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_handshake_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_handshake_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        supported_signature_schemes()
    }
}

#[derive(Debug)]
struct AcceptAnyClientVerifier;

impl ClientCertVerifier for AcceptAnyClientVerifier {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_handshake_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_handshake_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        supported_signature_schemes()
    }
}

fn supported_signature_schemes() -> Vec<SignatureScheme> {
    rustls::crypto::ring::default_provider()
        .signature_verification_algorithms
        .supported_schemes()
}

fn verify_tls12_handshake_signature(
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
    let supported = rustls::crypto::ring::default_provider().signature_verification_algorithms;
    rustls::crypto::verify_tls12_signature(message, cert, dss, &supported)
}

fn verify_tls13_handshake_signature(
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
    let supported = rustls::crypto::ring::default_provider().signature_verification_algorithms;
    rustls::crypto::verify_tls13_signature(message, cert, dss, &supported)
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::{tls_client_stream, tls_server_stream};
    use crate::{
        read_message, sync_with_device, write_message, AppKey, DeviceHandler, Identity, Message,
        Result, State, SyncListener,
    };

    struct AllowAllHandler {
        app_id: String,
        keys: crate::DeviceKeys,
        app_key: AppKey,
    }

    impl DeviceHandler for AllowAllHandler {
        fn app_id(&self) -> &str {
            &self.app_id
        }

        fn is_linked(&self, _identity: &Identity) -> bool {
            true
        }

        fn approve_link(&self, _identity: &Identity) -> Result<bool> {
            Ok(true)
        }

        fn device_keys(&self) -> Result<crate::DeviceKeys> {
            Ok(self.keys.clone())
        }

        fn app_key(&self) -> Result<AppKey> {
            Ok(self.app_key.clone())
        }
    }

    struct RecordingHandler {
        app_id: String,
        linked: Mutex<HashSet<String>>,
        approve: bool,
        keys: crate::DeviceKeys,
        fingerprints: Mutex<HashMap<String, String>>,
        app_key: Mutex<AppKey>,
    }

    impl RecordingHandler {
        fn new(app_id: &str, approve: bool, keys: crate::DeviceKeys) -> Self {
            let app_key = AppKey::generate().expect("app key");
            Self {
                app_id: app_id.to_string(),
                linked: Mutex::new(HashSet::new()),
                approve,
                keys,
                fingerprints: Mutex::new(HashMap::new()),
                app_key: Mutex::new(app_key),
            }
        }
    }

    impl DeviceHandler for RecordingHandler {
        fn app_id(&self) -> &str {
            &self.app_id
        }

        fn is_linked(&self, identity: &Identity) -> bool {
            self.linked
                .lock()
                .expect("linked lock")
                .contains(&identity.device_id)
        }

        fn approve_link(&self, identity: &Identity) -> Result<bool> {
            if self.approve {
                self.linked
                    .lock()
                    .expect("linked lock")
                    .insert(identity.device_id.clone());
                Ok(true)
            } else {
                Ok(false)
            }
        }

        fn device_keys(&self) -> Result<crate::DeviceKeys> {
            Ok(self.keys.clone())
        }

        fn app_key(&self) -> Result<AppKey> {
            Ok(self.app_key.lock().expect("app key lock").clone())
        }

        fn set_app_key(&self, app_key: &AppKey) -> Result<()> {
            *self.app_key.lock().expect("app key lock") = app_key.clone();
            Ok(())
        }

        fn is_linked_with_fingerprint(&self, identity: &Identity, fingerprint: &str) -> bool {
            if !self.is_linked(identity) {
                return false;
            }
            self.fingerprints
                .lock()
                .expect("fingerprints lock")
                .get(&identity.device_id)
                .map(|known| known == fingerprint)
                .unwrap_or(false)
        }

        fn approve_link_with_fingerprint(
            &self,
            identity: &Identity,
            fingerprint: &str,
        ) -> Result<bool> {
            if self.approve {
                self.linked
                    .lock()
                    .expect("linked lock")
                    .insert(identity.device_id.clone());
                self.fingerprints
                    .lock()
                    .expect("fingerprints lock")
                    .insert(identity.device_id.clone(), fingerprint.to_string());
                Ok(true)
            } else {
                Ok(false)
            }
        }
    }

    #[test]
    fn sync_round_trip_merges_entries() {
        let identity = Identity::new("listener", "com.example.app", "listener-user");
        let listener_keys = crate::DeviceKeys::generate(&identity).expect("listener keys");
        let app_key = AppKey::generate().expect("app key");
        let listener_state = Arc::new(Mutex::new(State::new("listener")));
        {
            let mut state = listener_state.lock().expect("state");
            state.set("alpha", b"one".to_vec());
        }

        let handler = Arc::new(AllowAllHandler {
            app_id: "com.example.app".to_string(),
            keys: listener_keys.clone(),
            app_key: app_key.clone(),
        });
        let listener = SyncListener::start(
            "127.0.0.1:0".parse().expect("addr"),
            identity,
            listener_state.clone(),
            handler,
        )
        .expect("listener start");

        let device_identity = Identity::new("device", "com.example.app", "device-user");
        let device_keys = crate::DeviceKeys::generate(&device_identity).expect("device keys");
        let device_app_key = app_key.clone();
        let mut device_state = State::new("device");
        device_state.set("beta", b"two".to_vec());

        sync_with_device(
            &device_identity,
            &mut device_state,
            listener.addr(),
            &device_keys,
            &device_app_key,
            |_, _| Ok(()),
        )
        .expect("sync");

        assert_eq!(device_state.get("alpha"), Some("one".as_bytes()));
        assert_eq!(device_state.get("beta"), Some("two".as_bytes()));

        let listener_snapshot = listener_state.lock().expect("state").snapshot();
        assert!(listener_snapshot.iter().any(|entry| entry.key == "beta"));

        listener.shutdown().expect("shutdown");
    }

    #[test]
    fn sync_with_device_errors_on_unexpected_message() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let listener_identity = Identity::new("listener", "com.example.app", "user");
        let listener_keys = crate::DeviceKeys::generate(&listener_identity).expect("listener keys");
        let listener_app_key = AppKey::generate().expect("app key");

        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept push");
            let stream = tls_server_stream(stream, &listener_keys).expect("tls server");
            let mut reader = std::io::BufReader::new(stream);

            let _ = read_message(&mut reader).expect("hello");
            write_message(
                reader.get_mut(),
                &Message::Hello {
                    identity: listener_identity.clone(),
                },
            )
            .expect("hello response");
            let _ = read_message(&mut reader).expect("snapshot");
            write_message(reader.get_mut(), &Message::Ack).expect("write ack");

            let (stream, _) = listener.accept().expect("accept pull");
            let stream = tls_server_stream(stream, &listener_keys).expect("tls server");
            let mut reader = std::io::BufReader::new(stream);

            let _ = read_message(&mut reader).expect("hello");
            write_message(
                reader.get_mut(),
                &Message::Hello {
                    identity: listener_identity,
                },
            )
            .expect("hello response");
            let _ = read_message(&mut reader).expect("request");
            write_message(reader.get_mut(), &Message::Ack).expect("write ack");
        });

        let mut state = State::new("device");
        let identity = Identity::new("device", "com.example.app", "user");
        let device_keys = crate::DeviceKeys::generate(&identity).expect("device keys");
        let result = sync_with_device(
            &identity,
            &mut state,
            addr,
            &device_keys,
            &listener_app_key,
            |_, _| Ok(()),
        );
        assert!(result.is_err());

        handle.join().expect("join");
    }

    #[test]
    fn link_request_updates_allowlist() {
        let identity = Identity::new("listener", "com.example.app", "listener-user");
        let listener_keys = crate::DeviceKeys::generate(&identity).expect("listener keys");
        let handler = Arc::new(RecordingHandler::new(
            "com.example.app",
            true,
            listener_keys,
        ));
        let listener_state = Arc::new(Mutex::new(State::new("listener")));
        let listener = SyncListener::start(
            "127.0.0.1:0".parse().expect("addr"),
            identity,
            listener_state,
            handler.clone(),
        )
        .expect("listener start");

        let device_identity = Identity::new("device", "com.example.app", "device-user");
        let device_keys = crate::DeviceKeys::generate(&device_identity).expect("device keys");
        let app_key = handler.app_key().expect("app key");
        let stream = TcpStream::connect(listener.addr()).expect("connect");
        let stream = tls_client_stream(stream, &device_keys).expect("tls client");
        let mut reader = std::io::BufReader::new(stream);

        write_message(
            reader.get_mut(),
            &Message::LinkRequest {
                identity: device_identity.clone(),
                app_key: Some(app_key.as_bytes().to_vec()),
                pairing_secret: None,
            },
        )
        .expect("link request");

        let response = read_message(&mut reader).expect("link response");
        match response {
            Message::LinkResponse { accepted, .. } => assert!(accepted),
            _ => panic!("expected link response"),
        }

        assert!(handler.is_linked_with_fingerprint(&device_identity, device_keys.fingerprint()));
        listener.shutdown().expect("shutdown");
    }

    #[test]
    fn sync_rejects_unlinked_device() {
        let identity = Identity::new("listener", "com.example.app", "listener-user");
        let listener_keys = crate::DeviceKeys::generate(&identity).expect("listener keys");
        let app_key = AppKey::generate().expect("app key");
        let handler = Arc::new(RecordingHandler::new(
            "com.example.app",
            false,
            listener_keys,
        ));
        let listener_state = Arc::new(Mutex::new(State::new("listener")));
        let listener = SyncListener::start(
            "127.0.0.1:0".parse().expect("addr"),
            identity,
            listener_state,
            handler,
        )
        .expect("listener start");

        let device_identity = Identity::new("device", "com.example.app", "user");
        let mut state = State::new("device");
        let device_keys = crate::DeviceKeys::generate(&device_identity).expect("device keys");
        let result = sync_with_device(
            &device_identity,
            &mut state,
            listener.addr(),
            &device_keys,
            &app_key,
            |_, _| Ok(()),
        );
        assert!(result.is_err());

        listener.shutdown().expect("shutdown");
    }
}
