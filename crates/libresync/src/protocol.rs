use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};

use crate::{Entry, Error, Identity, Result};

/// Wire protocol version spoken by this build.
///
/// - `1`: legacy full-snapshot exchange (`SnapshotRequest` / `Snapshot`), one
///   direction per connection.
/// - `2`: delta exchange (`SnapshotSince` / `Delta`), both directions on one
///   connection. Version 2 peers still answer version 1 requests.
pub const PROTOCOL_VERSION: u32 = 2;

fn legacy_protocol_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "type", content = "payload")]
pub enum Message {
    Hello {
        identity: Identity,
        /// Highest protocol version the sender understands. Absent in
        /// messages from pre-delta peers, which decodes as `1`.
        #[serde(default = "legacy_protocol_version")]
        protocol_version: u32,
    },
    LinkRequest {
        identity: Identity,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        app_key: Option<Vec<u8>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pairing_secret: Option<String>,
    },
    LinkResponse {
        identity: Identity,
        accepted: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        app_key: Option<Vec<u8>>,
    },
    SnapshotRequest,
    Snapshot {
        entries: Vec<Entry>,
    },
    Ack,
    /// Asks the peer for every entry it applied after its local apply
    /// sequence `clock` within history `epoch`. A zero clock, an unknown
    /// epoch, or a clock ahead of the peer's sequence yields a full snapshot.
    SnapshotSince {
        clock: u64,
        epoch: String,
    },
    /// Delta (or full snapshot when `full`) of encrypted entries.
    ///
    /// - `clock` / `epoch`: the sender's position after producing `entries`;
    ///   the receiver stores them as its cursor for the sender.
    /// - `acked` / `acked_epoch`: the sender's cursor for the receiver, i.e.
    ///   how far the sender has already received the receiver's entries. The
    ///   receiver uses it to size its own delta in the reverse direction.
    Delta {
        entries: Vec<Entry>,
        clock: u64,
        epoch: String,
        acked: u64,
        acked_epoch: String,
        full: bool,
    },
}

impl Message {
    pub fn hello(identity: Identity) -> Self {
        Message::Hello {
            identity,
            protocol_version: PROTOCOL_VERSION,
        }
    }
}

pub fn write_message<W: Write>(writer: &mut W, message: &Message) -> Result<()> {
    let serialized = serde_json::to_string(message)?;
    writer.write_all(serialized.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

pub fn read_message<R: BufRead>(reader: &mut R) -> Result<Message> {
    let mut line = String::new();
    let bytes_read = reader.read_line(&mut line)?;
    if bytes_read == 0 {
        return Err(Error::Protocol("unexpected EOF".to_string()));
    }
    let trimmed = line.trim_end_matches(['\n', '\r']);
    let message = serde_json::from_str(trimmed)?;
    Ok(message)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use crate::entry::LamportClock;
    use crate::protocol::{read_message, write_message, Message};
    use crate::{Entry, Identity};

    #[test]
    fn message_round_trip() {
        let message = Message::hello(Identity::new("device", "com.example.app", "user"));

        let mut buffer = Vec::new();
        write_message(&mut buffer, &message).expect("write message");

        let mut cursor = Cursor::new(buffer);
        let parsed = read_message(&mut cursor).expect("read message");
        assert_eq!(parsed, message);
    }

    #[test]
    fn legacy_hello_without_version_decodes_as_version_one() {
        let raw = b"{\"type\":\"Hello\",\"payload\":{\"identity\":{\"device_id\":\"d\",\"app_id\":\"a\",\"user_id\":\"u\"}}}\n".to_vec();
        let mut cursor = Cursor::new(raw);
        match read_message(&mut cursor).expect("read message") {
            Message::Hello {
                protocol_version, ..
            } => assert_eq!(protocol_version, 1),
            _ => panic!("expected hello"),
        }
    }

    #[test]
    fn delta_messages_round_trip() {
        for message in [
            Message::SnapshotSince {
                clock: 42,
                epoch: "e1".to_string(),
            },
            Message::Delta {
                entries: Vec::new(),
                clock: 7,
                epoch: "e1".to_string(),
                acked: 3,
                acked_epoch: "e2".to_string(),
                full: true,
            },
        ] {
            let mut buffer = Vec::new();
            write_message(&mut buffer, &message).expect("write message");
            let mut cursor = Cursor::new(buffer);
            let parsed = read_message(&mut cursor).expect("read message");
            assert_eq!(parsed, message);
        }
    }

    #[test]
    fn snapshot_message_round_trip() {
        let message = Message::Snapshot {
            entries: vec![Entry {
                key: "alpha".to_string(),
                value: b"one".to_vec(),
                clock: LamportClock {
                    counter: 1,
                    device_id: "device".to_string(),
                },
            }],
        };

        let mut buffer = Vec::new();
        write_message(&mut buffer, &message).expect("write message");

        let mut cursor = Cursor::new(buffer);
        let parsed = read_message(&mut cursor).expect("read message");
        assert_eq!(parsed, message);
    }

    #[test]
    fn read_message_rejects_empty_input() {
        let mut cursor = Cursor::new(Vec::<u8>::new());
        let error = read_message(&mut cursor).expect_err("expected error");
        assert!(error.to_string().contains("unexpected EOF"));
    }

    #[test]
    fn read_message_rejects_invalid_json() {
        let mut cursor = Cursor::new(b"{not json}\n".to_vec());
        let error = read_message(&mut cursor).expect_err("expected error");
        assert!(error.to_string().contains("serialization error"));
    }
}
