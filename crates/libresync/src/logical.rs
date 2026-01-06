use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use crate::{FieldValue, LogicalAdapter, MergePolicy, RecordState, RecordView, Result, SyncRecord};

pub struct InMemoryLogicalAdapter {
    id: String,
    namespace: String,
    records: Mutex<BTreeMap<(String, String, String), SyncRecord>>,
    merge_policies: HashMap<String, MergePolicy>,
}

impl InMemoryLogicalAdapter {
    pub fn new(id: impl Into<String>, namespace: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            namespace: namespace.into(),
            records: Mutex::new(BTreeMap::new()),
            merge_policies: HashMap::new(),
        }
    }

    pub fn with_merge_policy(mut self, field: impl Into<String>, policy: MergePolicy) -> Self {
        self.merge_policies.insert(field.into(), policy);
        self
    }

    pub fn upsert_record(&self, record: SyncRecord) {
        let key = record_key(&record);
        let mut records = self.records.lock().expect("records lock");
        records.insert(key, record);
    }

    pub fn record(&self, schema: &str, entity: &str, id: &str) -> Option<SyncRecord> {
        let records = self.records.lock().expect("records lock");
        records
            .get(&(schema.to_string(), entity.to_string(), id.to_string()))
            .cloned()
    }
}

impl LogicalAdapter for InMemoryLogicalAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    fn namespace(&self) -> &str {
        &self.namespace
    }

    fn merge_policy(&self, field: &str) -> MergePolicy {
        self.merge_policies
            .get(field)
            .cloned()
            .unwrap_or(MergePolicy::LastWriterWins)
    }

    fn load_records(&self, records: &mut RecordState) -> Result<()> {
        let stored = self.records.lock().expect("records lock");
        for record in stored.values().cloned() {
            records.apply(record)?;
        }
        Ok(())
    }

    fn apply_records(&self, records: &RecordView) -> Result<()> {
        let snapshot = records.snapshot()?;
        let mut store = self.records.lock().expect("records lock");
        store.clear();
        for record in snapshot {
            store.insert(record_key(&record), record);
        }
        Ok(())
    }

    fn apply_snapshot(&self, records: &mut RecordState, incoming: Vec<SyncRecord>) -> Result<usize> {
        let mut applied = 0;
        for record in incoming {
            let merged = match records.get(&record.schema, &record.entity, &record.id)? {
                Some(existing) => merge_records(existing, record, |field| self.merge_policy(field)),
                None => record,
            };
            if records.apply(merged)? {
                applied += 1;
            }
        }
        Ok(applied)
    }
}

fn record_key(record: &SyncRecord) -> (String, String, String) {
    (record.schema.clone(), record.entity.clone(), record.id.clone())
}

fn merge_records(
    existing: SyncRecord,
    incoming: SyncRecord,
    policy_for_field: impl Fn(&str) -> MergePolicy,
) -> SyncRecord {
    let incoming_newer = incoming.clock >= existing.clock;

    if existing.tombstone != incoming.tombstone {
        return if incoming_newer { incoming } else { existing };
    }

    if existing.tombstone && incoming.tombstone {
        return if incoming_newer { incoming } else { existing };
    }

    let mut fields = existing.fields.clone();
    for (field, incoming_value) in incoming.fields.into_iter() {
        let policy = policy_for_field(&field);
        let existing_value = fields.get(&field).cloned();
        let merged = merge_field(policy, existing_value, incoming_value, incoming_newer);
        fields.insert(field, merged);
    }

    let clock = if incoming_newer {
        incoming.clock.clone()
    } else {
        existing.clock.clone()
    };
    let updated_at = match (existing.updated_at, incoming.updated_at) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    };

    SyncRecord {
        schema: existing.schema,
        entity: existing.entity,
        id: existing.id,
        fields,
        tombstone: false,
        clock,
        updated_at,
    }
}

fn merge_field(
    policy: MergePolicy,
    existing: Option<FieldValue>,
    incoming: FieldValue,
    incoming_newer: bool,
) -> FieldValue {
    match policy {
        MergePolicy::LastWriterWins => {
            if incoming_newer {
                incoming
            } else {
                existing.unwrap_or(incoming)
            }
        }
        MergePolicy::SetUnion => match (existing, incoming) {
            (Some(FieldValue::List(mut left)), FieldValue::List(right)) => {
                for item in right {
                    if !left.contains(&item) {
                        left.push(item);
                    }
                }
                FieldValue::List(left)
            }
            (Some(FieldValue::Map(mut left)), FieldValue::Map(right)) => {
                for (key, value) in right {
                    if !left.contains_key(&key) {
                        left.insert(key, value);
                    }
                }
                FieldValue::Map(left)
            }
            (Some(value), incoming) => {
                if incoming_newer { incoming } else { value }
            }
            (None, incoming) => incoming,
        },
        MergePolicy::Counter => match (existing, incoming) {
            (Some(FieldValue::I64(left)), FieldValue::I64(right)) => FieldValue::I64(left + right),
            (Some(value), incoming) => {
                if incoming_newer { incoming } else { value }
            }
            (None, incoming) => incoming,
        },
        MergePolicy::ListAppend => match (existing, incoming) {
            (Some(FieldValue::List(mut left)), FieldValue::List(right)) => {
                left.extend(right);
                FieldValue::List(left)
            }
            (Some(value), incoming) => {
                if incoming_newer { incoming } else { value }
            }
            (None, incoming) => incoming,
        },
        MergePolicy::Custom(_) => {
            if incoming_newer {
                incoming
            } else {
                existing.unwrap_or(incoming)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::InMemoryLogicalAdapter;
    use crate::{FieldValue, LamportClock, LogicalAdapter, MergePolicy, RecordState, State, SyncRecord};
    use std::collections::BTreeMap;

    fn record(clock: u64, fields: BTreeMap<String, FieldValue>) -> SyncRecord {
        SyncRecord {
            schema: "schema".to_string(),
            entity: "Todo".to_string(),
            id: "1".to_string(),
            fields,
            tombstone: false,
            clock: LamportClock {
                counter: clock,
                device_id: "device".to_string(),
            },
            updated_at: None,
        }
    }

    #[test]
    fn merge_policy_set_union_combines_lists() {
        let adapter = InMemoryLogicalAdapter::new("logical", "app")
            .with_merge_policy("tags", MergePolicy::SetUnion);

        let mut state = State::new("device");
        let mut records = RecordState::new(&mut state, "app");
        records
            .apply(record(
                1,
                BTreeMap::from([(
                    "tags".to_string(),
                    FieldValue::List(vec![
                        FieldValue::String("a".to_string()),
                        FieldValue::String("b".to_string()),
                    ]),
                )]),
            ))
            .expect("apply");

        adapter
            .apply_snapshot(
                &mut records,
                vec![record(
                    2,
                    BTreeMap::from([(
                        "tags".to_string(),
                        FieldValue::List(vec![
                            FieldValue::String("b".to_string()),
                            FieldValue::String("c".to_string()),
                        ]),
                    )]),
                )],
            )
            .expect("merge");

        let merged = records
            .get("schema", "Todo", "1")
            .expect("get")
            .expect("record");
        let tags = merged.fields.get("tags").expect("tags");
        assert_eq!(
            tags,
            &FieldValue::List(vec![
                FieldValue::String("a".to_string()),
                FieldValue::String("b".to_string()),
                FieldValue::String("c".to_string()),
            ])
        );
    }

    #[test]
    fn merge_policy_counter_sums() {
        let adapter = InMemoryLogicalAdapter::new("logical", "app")
            .with_merge_policy("count", MergePolicy::Counter);

        let mut state = State::new("device");
        let mut records = RecordState::new(&mut state, "app");
        records
            .apply(record(
                1,
                BTreeMap::from([("count".to_string(), FieldValue::I64(2))]),
            ))
            .expect("apply");

        adapter
            .apply_snapshot(
                &mut records,
                vec![record(
                    2,
                    BTreeMap::from([("count".to_string(), FieldValue::I64(3))]),
                )],
            )
            .expect("merge");

        let merged = records
            .get("schema", "Todo", "1")
            .expect("get")
            .expect("record");
        assert_eq!(
            merged.fields.get("count"),
            Some(&FieldValue::I64(5))
        );
    }

    #[test]
    fn merge_policy_list_append_concatenates() {
        let adapter = InMemoryLogicalAdapter::new("logical", "app")
            .with_merge_policy("items", MergePolicy::ListAppend);

        let mut state = State::new("device");
        let mut records = RecordState::new(&mut state, "app");
        records
            .apply(record(
                1,
                BTreeMap::from([(
                    "items".to_string(),
                    FieldValue::List(vec![FieldValue::String("a".to_string())]),
                )]),
            ))
            .expect("apply");

        adapter
            .apply_snapshot(
                &mut records,
                vec![record(
                    2,
                    BTreeMap::from([(
                        "items".to_string(),
                        FieldValue::List(vec![FieldValue::String("b".to_string())]),
                    )]),
                )],
            )
            .expect("merge");

        let merged = records
            .get("schema", "Todo", "1")
            .expect("get")
            .expect("record");
        assert_eq!(
            merged.fields.get("items"),
            Some(&FieldValue::List(vec![
                FieldValue::String("a".to_string()),
                FieldValue::String("b".to_string()),
            ]))
        );
    }

    #[test]
    fn merge_policy_lww_keeps_newer_value() {
        let adapter = InMemoryLogicalAdapter::new("logical", "app");

        let mut state = State::new("device");
        let mut records = RecordState::new(&mut state, "app");
        records
            .apply(record(
                5,
                BTreeMap::from([(
                    "title".to_string(),
                    FieldValue::String("newer".to_string()),
                )]),
            ))
            .expect("apply");

        adapter
            .apply_snapshot(
                &mut records,
                vec![record(
                    2,
                    BTreeMap::from([(
                        "title".to_string(),
                        FieldValue::String("older".to_string()),
                    )]),
                )],
            )
            .expect("merge");

        let merged = records
            .get("schema", "Todo", "1")
            .expect("get")
            .expect("record");
        assert_eq!(
            merged.fields.get("title"),
            Some(&FieldValue::String("newer".to_string()))
        );
    }
}
