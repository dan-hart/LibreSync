use std::io::{BufReader, BufWriter};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::protocol::{read_message, write_message, Message};
use crate::{DeviceHandler, Error, Identity, Result, State};

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

        let (shutdown_sender, shutdown_receiver) = mpsc::channel();
        let handle = thread::spawn(move || loop {
            if shutdown_receiver.try_recv().is_ok() {
                break Ok(());
            }

            match listener.accept() {
                Ok((stream, _)) => {
                    if let Err(error) =
                        handle_connection(stream, &identity, handler.as_ref(), &state)
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
    device_check: F,
) -> Result<Identity>
where
    F: Fn(&Identity) -> Result<()>,
{
    let remote_identity = push_snapshot(identity, state, device, &device_check)?;
    pull_snapshot(identity, state, device, &remote_identity)?;
    Ok(remote_identity)
}

fn handle_connection(
    stream: TcpStream,
    identity: &Identity,
    handler: &dyn DeviceHandler,
    state: &Arc<Mutex<State>>,
) -> Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = BufWriter::new(stream);

    match read_message(&mut reader)? {
        Message::PairRequest { identity: device_identity } => {
            if !device_identity.matches_app(handler.app_id()) {
                write_message(
                    &mut writer,
                    &Message::PairResponse {
                        identity: identity.clone(),
                        accepted: false,
                    },
                )?;
                return Ok(());
            }
            let accepted = handler.approve_pair(&device_identity)?;
            write_message(
                &mut writer,
                &Message::PairResponse {
                    identity: identity.clone(),
                    accepted,
                },
            )?;
        }
        Message::Hello { identity: device_identity } => {
            if !device_identity.matches_app(handler.app_id()) {
                return Err(Error::Protocol("app id mismatch".to_string()));
            }
            if !handler.is_paired(&device_identity) {
                return Err(Error::Protocol("device not paired".to_string()));
            }
            write_message(&mut writer, &Message::Hello { identity: identity.clone() })?;

            match read_message(&mut reader)? {
                Message::SnapshotRequest => {
                    let snapshot = {
                        let state = state.lock().expect("state poisoned");
                        state.snapshot()
                    };
                    write_message(&mut writer, &Message::Snapshot { entries: snapshot })?;
                }
                Message::Snapshot { entries } => {
                    let mut state = state.lock().expect("state poisoned");
                    state.merge_snapshot(entries);
                    write_message(&mut writer, &Message::Ack)?;
                }
                _ => {
                    return Err(Error::Protocol(
                        "expected snapshot request or snapshot".to_string(),
                    ))
                }
            }
        }
        _ => return Err(Error::Protocol("expected hello or pair request".to_string())),
    }

    Ok(())
}

fn push_snapshot<F>(
    identity: &Identity,
    state: &State,
    device: SocketAddr,
    device_check: &F,
) -> Result<Identity>
where
    F: Fn(&Identity) -> Result<()>,
{
    let stream = TcpStream::connect_timeout(&device, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = BufWriter::new(stream);

    write_message(&mut writer, &Message::Hello { identity: identity.clone() })?;
    let remote_identity = read_hello(&mut reader, identity)?;
    device_check(&remote_identity)?;

    let snapshot = state.snapshot();
    write_message(&mut writer, &Message::Snapshot { entries: snapshot })?;

    let ack = read_message(&mut reader)?;
    if ack != Message::Ack {
        return Err(Error::Protocol("expected ack".to_string()));
    }

    Ok(remote_identity)
}

fn pull_snapshot(
    identity: &Identity,
    state: &mut State,
    device: SocketAddr,
    expected_remote: &Identity,
) -> Result<()> {
    let stream = TcpStream::connect_timeout(&device, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;

    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = BufWriter::new(stream);

    write_message(&mut writer, &Message::Hello { identity: identity.clone() })?;
    let remote_identity = read_hello(&mut reader, identity)?;
    if remote_identity != *expected_remote {
        return Err(Error::Protocol("device identity changed".to_string()));
    }
    write_message(&mut writer, &Message::SnapshotRequest)?;

    let response = read_message(&mut reader)?;
            let entries = match response {
                Message::Snapshot { entries } => entries,
                _ => return Err(Error::Protocol("expected snapshot".to_string())),
            };
            state.merge_snapshot(entries);

    Ok(())
}

fn read_hello(reader: &mut BufReader<TcpStream>, identity: &Identity) -> Result<Identity> {
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    use crate::{
        read_message, sync_with_device, write_message, DeviceHandler, Identity, Message, Result,
        State, SyncListener,
    };

    struct AllowAllHandler {
        app_id: String,
    }

    impl DeviceHandler for AllowAllHandler {
        fn app_id(&self) -> &str {
            &self.app_id
        }

        fn is_paired(&self, _identity: &Identity) -> bool {
            true
        }

        fn approve_pair(&self, _identity: &Identity) -> Result<bool> {
            Ok(true)
        }
    }

    struct RecordingHandler {
        app_id: String,
        paired: Mutex<HashSet<String>>,
        approve: bool,
    }

    impl RecordingHandler {
        fn new(app_id: &str, approve: bool) -> Self {
            Self {
                app_id: app_id.to_string(),
                paired: Mutex::new(HashSet::new()),
                approve,
            }
        }
    }

    impl DeviceHandler for RecordingHandler {
        fn app_id(&self) -> &str {
            &self.app_id
        }

        fn is_paired(&self, identity: &Identity) -> bool {
            self.paired
                .lock()
                .expect("paired lock")
                .contains(&identity.device_id)
        }

        fn approve_pair(&self, identity: &Identity) -> Result<bool> {
            if self.approve {
                self.paired
                    .lock()
                    .expect("paired lock")
                    .insert(identity.device_id.clone());
                Ok(true)
            } else {
                Ok(false)
            }
        }
    }

    #[test]
    fn sync_round_trip_merges_entries() {
        let identity = Identity::new("listener", "com.example.app", "listener-user");
        let listener_state = Arc::new(Mutex::new(State::new("listener")));
        {
            let mut state = listener_state.lock().expect("state");
            state.set("alpha", b"one".to_vec());
        }

        let handler = Arc::new(AllowAllHandler {
            app_id: "com.example.app".to_string(),
        });
        let listener = SyncListener::start(
            "127.0.0.1:0".parse().expect("addr"),
            identity,
            listener_state.clone(),
            handler,
        )
        .expect("listener start");

        let device_identity = Identity::new("device", "com.example.app", "device-user");
        let mut device_state = State::new("device");
        device_state.set("beta", b"two".to_vec());

        sync_with_device(&device_identity, &mut device_state, listener.addr(), |_| Ok(()))
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

        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept push");
            let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
            let mut writer = std::io::BufWriter::new(stream);

            let _ = read_message(&mut reader).expect("hello");
            write_message(
                &mut writer,
                &Message::Hello {
                    identity: Identity::new("listener", "com.example.app", "user"),
                },
            )
            .expect("hello response");
            let _ = read_message(&mut reader).expect("snapshot");
            write_message(&mut writer, &Message::Ack).expect("write ack");

            let (stream, _) = listener.accept().expect("accept pull");
            let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
            let mut writer = std::io::BufWriter::new(stream);

            let _ = read_message(&mut reader).expect("hello");
            write_message(
                &mut writer,
                &Message::Hello {
                    identity: Identity::new("listener", "com.example.app", "user"),
                },
            )
            .expect("hello response");
            let _ = read_message(&mut reader).expect("request");
            write_message(&mut writer, &Message::Ack).expect("write ack");
        });

        let mut state = State::new("device");
        let identity = Identity::new("device", "com.example.app", "user");
        let result = sync_with_device(&identity, &mut state, addr, |_| Ok(()));
        assert!(result.is_err());

        handle.join().expect("join");
    }

    #[test]
    fn pair_request_updates_allowlist() {
        let handler = Arc::new(RecordingHandler::new("com.example.app", true));
        let listener_state = Arc::new(Mutex::new(State::new("listener")));
        let identity = Identity::new("listener", "com.example.app", "listener-user");
        let listener = SyncListener::start(
            "127.0.0.1:0".parse().expect("addr"),
            identity,
            listener_state,
            handler.clone(),
        )
        .expect("listener start");

        let device_identity = Identity::new("device", "com.example.app", "device-user");
        let stream = TcpStream::connect(listener.addr()).expect("connect");
        let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
        let mut writer = std::io::BufWriter::new(stream);

        write_message(
            &mut writer,
            &Message::PairRequest {
                identity: device_identity.clone(),
            },
        )
        .expect("pair request");

        let response = read_message(&mut reader).expect("pair response");
        match response {
            Message::PairResponse { accepted, .. } => assert!(accepted),
            _ => panic!("expected pair response"),
        }

        assert!(handler.is_paired(&device_identity));
        listener.shutdown().expect("shutdown");
    }

    #[test]
    fn sync_rejects_unpaired_device() {
        let handler = Arc::new(RecordingHandler::new("com.example.app", false));
        let listener_state = Arc::new(Mutex::new(State::new("listener")));
        let identity = Identity::new("listener", "com.example.app", "listener-user");
        let listener = SyncListener::start(
            "127.0.0.1:0".parse().expect("addr"),
            identity,
            listener_state,
            handler,
        )
        .expect("listener start");

        let device_identity = Identity::new("device", "com.example.app", "user");
        let mut state = State::new("device");
        let result = sync_with_device(&device_identity, &mut state, listener.addr(), |_| Ok(()));
        assert!(result.is_err());

        listener.shutdown().expect("shutdown");
    }
}
