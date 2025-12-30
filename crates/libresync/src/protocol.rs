use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};

use crate::{Entry, Error, Identity, Result};

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "type", content = "payload")]
pub enum Message {
    Hello { identity: Identity },
    PairRequest { identity: Identity },
    PairResponse { identity: Identity, accepted: bool },
    SnapshotRequest,
    Snapshot { entries: Vec<Entry> },
    Ack,
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
        let message = Message::Hello {
            identity: Identity::new("device", "com.example.app", "user"),
        };

        let mut buffer = Vec::new();
        write_message(&mut buffer, &message).expect("write message");

        let mut cursor = Cursor::new(buffer);
        let parsed = read_message(&mut cursor).expect("read message");
        assert_eq!(parsed, message);
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
