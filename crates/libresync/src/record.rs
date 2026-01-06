use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Entry, Error, LamportClock, Result, State};

const RECORD_PREFIX: &str = "record";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RecordKeyParts {
    pub namespace: String,
    pub schema: String,
    pub entity: String,
    pub id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "value")]
pub enum FieldValue {
    Null,
    Bool(bool),
    I64(i64),
    F64(f64),
    String(String),
    Bytes(Vec<u8>),
    List(Vec<FieldValue>),
    Map(BTreeMap<String, FieldValue>),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SyncRecord {
    pub schema: String,
    pub entity: String,
    pub id: String,
    pub fields: BTreeMap<String, FieldValue>,
    pub tombstone: bool,
    pub clock: LamportClock,
    pub updated_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MergePolicy {
    LastWriterWins,
    SetUnion,
    Counter,
    ListAppend,
    Custom(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct RecordPayload {
    #[serde(default)]
    fields: BTreeMap<String, FieldValue>,
    #[serde(default)]
    tombstone: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated_at: Option<u64>,
}

pub struct RecordState<'a> {
    state: &'a mut State,
    namespace: String,
}

pub struct RecordView<'a> {
    state: &'a State,
    namespace: String,
}

impl<'a> RecordState<'a> {
    pub fn new(state: &'a mut State, namespace: impl Into<String>) -> Self {
        Self {
            state,
            namespace: namespace.into(),
        }
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn set(&mut self, record: SyncRecord) -> Result<Entry> {
        let key = record_entry_key(&self.namespace, &record.schema, &record.entity, &record.id);
        let payload = RecordPayload {
            fields: record.fields,
            tombstone: record.tombstone,
            updated_at: record.updated_at,
        };
        let value = serde_json::to_vec(&payload)?;
        Ok(self.state.set(key, value))
    }

    pub fn apply(&mut self, record: SyncRecord) -> Result<bool> {
        let entry = record_to_entry(&self.namespace, &record)?;
        Ok(self.state.apply_entry(entry))
    }

    pub fn snapshot(&self) -> Result<Vec<SyncRecord>> {
        let view = RecordView::new(self.state, self.namespace.clone());
        view.snapshot()
    }

    pub fn merge_snapshot(
        &mut self,
        incoming: impl IntoIterator<Item = SyncRecord>,
    ) -> Result<usize> {
        let mut applied = 0;
        for record in incoming {
            if self.apply(record)? {
                applied += 1;
            }
        }
        Ok(applied)
    }
}

impl<'a> RecordView<'a> {
    pub fn new(state: &'a State, namespace: impl Into<String>) -> Self {
        Self {
            state,
            namespace: namespace.into(),
        }
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn snapshot(&self) -> Result<Vec<SyncRecord>> {
        let mut records = Vec::new();
        for entry in self.state.snapshot() {
            if let Some(record) = entry_to_record(&entry, &self.namespace)? {
                records.push(record);
            }
        }
        Ok(records)
    }
}

pub fn record_entry_key(
    namespace: &str,
    schema: &str,
    entity: &str,
    id: &str,
) -> String {
    format!(
        "{}:{}:{}:{}:{}",
        RECORD_PREFIX,
        escape_component(namespace),
        escape_component(schema),
        escape_component(entity),
        escape_component(id)
    )
}

pub fn parse_record_key(key: &str) -> Result<Option<RecordKeyParts>> {
    if !key.starts_with(RECORD_PREFIX) {
        return Ok(None);
    }

    let mut parts = key.splitn(5, ':');
    let prefix = parts.next().unwrap_or_default();
    if prefix != RECORD_PREFIX {
        return Ok(None);
    }

    let namespace = parts
        .next()
        .ok_or_else(|| Error::Protocol("record key missing namespace".to_string()))?;
    let schema = parts
        .next()
        .ok_or_else(|| Error::Protocol("record key missing schema".to_string()))?;
    let entity = parts
        .next()
        .ok_or_else(|| Error::Protocol("record key missing entity".to_string()))?;
    let id = parts
        .next()
        .ok_or_else(|| Error::Protocol("record key missing id".to_string()))?;

    Ok(Some(RecordKeyParts {
        namespace: unescape_component(namespace)?,
        schema: unescape_component(schema)?,
        entity: unescape_component(entity)?,
        id: unescape_component(id)?,
    }))
}

pub fn record_to_entry(namespace: &str, record: &SyncRecord) -> Result<Entry> {
    let key = record_entry_key(namespace, &record.schema, &record.entity, &record.id);
    let payload = RecordPayload {
        fields: record.fields.clone(),
        tombstone: record.tombstone,
        updated_at: record.updated_at,
    };
    let value = serde_json::to_vec(&payload)?;
    Ok(Entry {
        key,
        value,
        clock: record.clock.clone(),
    })
}

pub fn entry_to_record(entry: &Entry, namespace: &str) -> Result<Option<SyncRecord>> {
    let parts = match parse_record_key(&entry.key)? {
        Some(parts) if parts.namespace == namespace => parts,
        Some(_) => return Ok(None),
        None => return Ok(None),
    };

    let payload: RecordPayload = serde_json::from_slice(&entry.value)?;
    Ok(Some(SyncRecord {
        schema: parts.schema,
        entity: parts.entity,
        id: parts.id,
        fields: payload.fields,
        tombstone: payload.tombstone,
        updated_at: payload.updated_at,
        clock: entry.clock.clone(),
    }))
}

fn escape_component(input: &str) -> String {
    let mut out = String::new();
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~' => out.push(*byte as char),
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

fn unescape_component(input: &str) -> Result<String> {
    let mut bytes = Vec::new();
    let mut chars = input.as_bytes().iter().copied();

    while let Some(byte) = chars.next() {
        if byte == b'%' {
            let hi = chars
                .next()
                .ok_or_else(|| Error::Protocol("invalid escape sequence".to_string()))?;
            let lo = chars
                .next()
                .ok_or_else(|| Error::Protocol("invalid escape sequence".to_string()))?;
            let hex = [hi, lo];
            let hex_str = std::str::from_utf8(&hex)
                .map_err(|_| Error::Protocol("invalid escape sequence".to_string()))?;
            let value = u8::from_str_radix(hex_str, 16)
                .map_err(|_| Error::Protocol("invalid escape sequence".to_string()))?;
            bytes.push(value);
        } else {
            bytes.push(byte);
        }
    }

    String::from_utf8(bytes).map_err(|_| Error::Protocol("invalid utf-8 in record key".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_key_round_trip() {
        let key = record_entry_key("app", "schema/1", "Todo", "item:1");
        let parsed = parse_record_key(&key).expect("parse").expect("parts");
        assert_eq!(parsed.namespace, "app");
        assert_eq!(parsed.schema, "schema/1");
        assert_eq!(parsed.entity, "Todo");
        assert_eq!(parsed.id, "item:1");
    }

    #[test]
    fn entry_round_trip() {
        let record = SyncRecord {
            schema: "schema".to_string(),
            entity: "Todo".to_string(),
            id: "1".to_string(),
            fields: BTreeMap::from([(
                "title".to_string(),
                FieldValue::String("Hello".to_string()),
            )]),
            tombstone: false,
            clock: LamportClock {
                counter: 1,
                device_id: "device".to_string(),
            },
            updated_at: Some(123),
        };

        let entry = record_to_entry("app", &record).expect("entry");
        let decoded = entry_to_record(&entry, "app").expect("decode").expect("record");
        assert_eq!(decoded, record);
    }

    #[test]
    fn parse_record_key_returns_none_for_non_record() {
        let result = parse_record_key("alpha").expect("parse");
        assert!(result.is_none());
    }

    #[test]
    fn entry_to_record_returns_none_for_namespace_mismatch() {
        let record = SyncRecord {
            schema: "schema".to_string(),
            entity: "Todo".to_string(),
            id: "1".to_string(),
            fields: BTreeMap::new(),
            tombstone: false,
            clock: LamportClock {
                counter: 1,
                device_id: "device".to_string(),
            },
            updated_at: None,
        };
        let entry = record_to_entry("app", &record).expect("entry");
        let decoded = entry_to_record(&entry, "other").expect("decode");
        assert!(decoded.is_none());
    }

    #[test]
    fn parse_record_key_errors_on_bad_escape() {
        let result = parse_record_key("record:app:schema:%ZZ:1");
        assert!(result.is_err());
    }

    #[test]
    fn parse_record_key_errors_on_invalid_utf8() {
        let result = parse_record_key("record:app:schema:%FF:1");
        assert!(result.is_err());
    }

    #[test]
    fn entry_to_record_errors_on_invalid_payload() {
        let entry = Entry {
            key: record_entry_key("app", "schema", "Todo", "1"),
            value: b"{".to_vec(),
            clock: LamportClock {
                counter: 1,
                device_id: "device".to_string(),
            },
        };
        let result = entry_to_record(&entry, "app");
        assert!(result.is_err());
    }
}
