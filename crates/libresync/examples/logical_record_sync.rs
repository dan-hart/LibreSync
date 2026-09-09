use std::collections::BTreeMap;

use libresync::{
    FieldValue, FileLogicalAdapter, LamportClock, LogicalAdapter, RecordState, RecordView, State,
    SyncRecord,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let namespace = "com.example.notes";
    let adapter = FileLogicalAdapter::new("records", namespace, "./records.json");

    let mut state = State::new("device-a");
    let mut records = RecordState::new(&mut state, namespace);
    records.set(SyncRecord {
        schema: "notes".to_string(),
        entity: "Note".to_string(),
        id: "note-1".to_string(),
        fields: BTreeMap::from([
            ("title".to_string(), FieldValue::String("Hello".to_string())),
            ("done".to_string(), FieldValue::Bool(false)),
        ]),
        tombstone: false,
        clock: LamportClock {
            counter: 1,
            device_id: "device-a".to_string(),
        },
        updated_at: None,
        field_clocks: Default::default(),
    })?;

    adapter.apply_records(&RecordView::new(&state, namespace))?;
    println!("Wrote logical records to ./records.json");
    Ok(())
}
