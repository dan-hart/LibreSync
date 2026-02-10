#[cfg(feature = "sqlite-logical")]
use rusqlite::types::Value as SqlValue;
#[cfg(feature = "sqlite-logical")]
use rusqlite::{params, params_from_iter, Connection};

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::{
    logical::apply_snapshot_with_policy, Error, FieldValue, LamportClock, LogicalAdapter,
    MergePolicy, RecordState, RecordView, Result, SyncRecord,
};

use serde::{Deserialize, Serialize};

type CustomMergeHandler =
    dyn Fn(&str, Option<&FieldValue>, &FieldValue, bool) -> Option<FieldValue> + Send + Sync;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SqliteLogicalEncoding {
    Plain,
    Bool,
    Json,
    JsonValue,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SqliteLogicalField {
    column: String,
    field: String,
    encoding: SqliteLogicalEncoding,
    #[serde(default)]
    merge_policy: Option<MergePolicy>,
}

impl SqliteLogicalField {
    pub fn new(column: impl Into<String>, field: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            field: field.into(),
            encoding: SqliteLogicalEncoding::Plain,
            merge_policy: None,
        }
    }

    pub fn json(column: impl Into<String>, field: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            field: field.into(),
            encoding: SqliteLogicalEncoding::Json,
            merge_policy: None,
        }
    }

    pub fn boolean(column: impl Into<String>, field: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            field: field.into(),
            encoding: SqliteLogicalEncoding::Bool,
            merge_policy: None,
        }
    }

    pub fn json_value(column: impl Into<String>, field: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            field: field.into(),
            encoding: SqliteLogicalEncoding::JsonValue,
            merge_policy: None,
        }
    }

    pub fn with_merge_policy(mut self, policy: MergePolicy) -> Self {
        self.merge_policy = Some(policy);
        self
    }

    pub fn with_encoding(mut self, encoding: SqliteLogicalEncoding) -> Self {
        self.encoding = encoding;
        self
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SqliteLogicalMapping {
    data_table: String,
    id_column: String,
    schema: String,
    entity: String,
    fields: Vec<SqliteLogicalField>,
    meta_table: String,
    #[serde(default)]
    default_merge_policy: Option<MergePolicy>,
}

impl SqliteLogicalMapping {
    pub fn new(
        data_table: impl Into<String>,
        id_column: impl Into<String>,
        schema: impl Into<String>,
        entity: impl Into<String>,
    ) -> Self {
        let table = data_table.into();
        Self {
            meta_table: format!("{table}_libresync_meta"),
            data_table: table,
            id_column: id_column.into(),
            schema: schema.into(),
            entity: entity.into(),
            fields: Vec::new(),
            default_merge_policy: None,
        }
    }

    pub fn with_field(mut self, column: impl Into<String>, field: impl Into<String>) -> Self {
        self.fields.push(SqliteLogicalField::new(column, field));
        self
    }

    pub fn with_json_field(mut self, column: impl Into<String>, field: impl Into<String>) -> Self {
        self.fields.push(SqliteLogicalField::json(column, field));
        self
    }

    pub fn with_bool_field(mut self, column: impl Into<String>, field: impl Into<String>) -> Self {
        self.fields.push(SqliteLogicalField::boolean(column, field));
        self
    }

    pub fn with_json_value_field(
        mut self,
        column: impl Into<String>,
        field: impl Into<String>,
    ) -> Self {
        self.fields
            .push(SqliteLogicalField::json_value(column, field));
        self
    }

    pub fn with_field_def(mut self, field: SqliteLogicalField) -> Self {
        self.fields.push(field);
        self
    }

    pub fn with_meta_table(mut self, table: impl Into<String>) -> Self {
        self.meta_table = table.into();
        self
    }

    pub fn with_default_merge_policy(mut self, policy: MergePolicy) -> Self {
        self.default_merge_policy = Some(policy);
        self
    }

    fn policy_for(&self, field: &str) -> Option<MergePolicy> {
        self.fields
            .iter()
            .find(|entry| entry.field == field)
            .and_then(|entry| entry.merge_policy.clone())
            .or_else(|| self.default_merge_policy.clone())
    }
}

pub struct SqliteLogicalAdapter {
    id: String,
    namespace: String,
    path: PathBuf,
    table: String,
    merge_policies: BTreeMap<String, MergePolicy>,
    custom_merge_handlers: HashMap<String, Arc<CustomMergeHandler>>,
    mappings: Vec<SqliteLogicalMapping>,
}

impl SqliteLogicalAdapter {
    pub fn new(
        id: impl Into<String>,
        namespace: impl Into<String>,
        path: impl Into<PathBuf>,
        table: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            namespace: namespace.into(),
            path: path.into(),
            table: table.into(),
            merge_policies: BTreeMap::new(),
            custom_merge_handlers: HashMap::new(),
            mappings: Vec::new(),
        }
    }

    pub fn with_merge_policy(mut self, field: impl Into<String>, policy: MergePolicy) -> Self {
        self.merge_policies.insert(field.into(), policy);
        self
    }

    pub fn with_custom_merge_handler<F>(
        mut self,
        policy_name: impl Into<String>,
        handler: F,
    ) -> Self
    where
        F: Fn(&str, Option<&FieldValue>, &FieldValue, bool) -> Option<FieldValue>
            + Send
            + Sync
            + 'static,
    {
        self.custom_merge_handlers
            .insert(policy_name.into(), Arc::new(handler));
        self
    }

    pub fn with_mapping(mut self, mapping: SqliteLogicalMapping) -> Self {
        self.mappings.push(mapping);
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn ensure_table(&self, conn: &Connection) -> Result<()> {
        let sql = format!(
            "CREATE TABLE IF NOT EXISTS {} (\
             schema TEXT NOT NULL,\
             entity TEXT NOT NULL,\
             id TEXT NOT NULL,\
             fields TEXT NOT NULL,\
             tombstone INTEGER NOT NULL,\
             updated_at INTEGER,\
             clock_counter INTEGER NOT NULL,\
             clock_device TEXT NOT NULL,\
             PRIMARY KEY (schema, entity, id)\
             )",
            self.table
        );
        conn.execute(&sql, [])
            .map_err(|error| Error::Protocol(error.to_string()))?;
        Ok(())
    }

    fn open(&self) -> Result<Connection> {
        Connection::open(&self.path).map_err(|error| Error::Protocol(error.to_string()))
    }

    fn ensure_meta_table(&self, conn: &Connection, mapping: &SqliteLogicalMapping) -> Result<()> {
        let sql = format!(
            "CREATE TABLE IF NOT EXISTS {} (\
             schema TEXT NOT NULL,\
             entity TEXT NOT NULL,\
             record_id TEXT NOT NULL,\
             tombstone INTEGER NOT NULL,\
             updated_at INTEGER,\
             clock_counter INTEGER NOT NULL,\
             clock_device TEXT NOT NULL,\
             PRIMARY KEY (schema, entity, record_id)\
             )",
            mapping.meta_table
        );
        conn.execute(&sql, [])
            .map_err(|error| Error::Protocol(error.to_string()))?;
        Ok(())
    }

    fn load_records_mapped(
        &self,
        records: &mut RecordState,
        mapping: &SqliteLogicalMapping,
    ) -> Result<()> {
        let conn = self.open()?;
        self.ensure_meta_table(&conn, mapping)?;

        let meta_sql = format!(
            "SELECT record_id, tombstone, updated_at, clock_counter, clock_device \
             FROM {} WHERE schema = ?1 AND entity = ?2",
            mapping.meta_table
        );
        let mut meta_stmt = conn
            .prepare(&meta_sql)
            .map_err(|error| Error::Protocol(error.to_string()))?;
        let meta_rows = meta_stmt
            .query_map(params![&mapping.schema, &mapping.entity], |row| {
                let record_id: String = row.get(0)?;
                let tombstone: i64 = row.get(1)?;
                let updated_at: Option<i64> = row.get(2)?;
                let clock_counter: i64 = row.get(3)?;
                let clock_device: String = row.get(4)?;
                Ok((
                    record_id,
                    tombstone != 0,
                    updated_at.map(|value| value.max(0) as u64),
                    LamportClock {
                        counter: clock_counter.max(0) as u64,
                        device_id: clock_device,
                    },
                ))
            })
            .map_err(|error| Error::Protocol(error.to_string()))?;

        let mut meta_map = BTreeMap::new();
        for row in meta_rows {
            let (record_id, tombstone, updated_at, clock) =
                row.map_err(|error| Error::Protocol(error.to_string()))?;
            meta_map.insert(record_id, (tombstone, updated_at, clock));
        }

        let mut columns = Vec::with_capacity(1 + mapping.fields.len());
        columns.push(mapping.id_column.clone());
        for field in &mapping.fields {
            columns.push(field.column.clone());
        }
        let sql = format!("SELECT {} FROM {}", columns.join(", "), mapping.data_table);
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|error| Error::Protocol(error.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                let id_value: SqlValue = row.get(0)?;
                let record_id = sqlite_value_to_string(id_value);
                let mut fields = BTreeMap::new();
                for (index, field) in mapping.fields.iter().enumerate() {
                    let value: SqlValue = row.get(index + 1)?;
                    let field_value = sqlite_value_to_field(value, &field.encoding);
                    fields.insert(field.field.clone(), field_value);
                }
                Ok((record_id, fields))
            })
            .map_err(|error| Error::Protocol(error.to_string()))?;

        let mut seen = HashSet::new();
        for row in rows {
            let (record_id, fields) = row.map_err(|error| Error::Protocol(error.to_string()))?;
            seen.insert(record_id.clone());
            let (tombstone, updated_at, clock) =
                meta_map.get(&record_id).cloned().unwrap_or_else(|| {
                    (
                        false,
                        None,
                        LamportClock {
                            counter: 0,
                            device_id: "bootstrap".to_string(),
                        },
                    )
                });
            let record = SyncRecord {
                schema: mapping.schema.clone(),
                entity: mapping.entity.clone(),
                id: record_id,
                fields,
                tombstone,
                clock,
                updated_at,
            };
            records.apply(record)?;
        }

        for (record_id, (tombstone, updated_at, clock)) in meta_map {
            if seen.contains(&record_id) {
                continue;
            }
            let record = SyncRecord {
                schema: mapping.schema.clone(),
                entity: mapping.entity.clone(),
                id: record_id,
                fields: BTreeMap::new(),
                tombstone,
                clock,
                updated_at,
            };
            records.apply(record)?;
        }

        Ok(())
    }

    fn apply_records_mapped(
        &self,
        records: &RecordView,
        mapping: &SqliteLogicalMapping,
    ) -> Result<()> {
        let mut conn = self.open()?;
        self.ensure_meta_table(&conn, mapping)?;
        let tx = conn
            .transaction()
            .map_err(|error| Error::Protocol(error.to_string()))?;

        let meta_sql = format!(
            "INSERT INTO {} (schema, entity, record_id, tombstone, updated_at, clock_counter, clock_device) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(schema, entity, record_id) DO UPDATE SET \
             tombstone = excluded.tombstone, \
             updated_at = excluded.updated_at, \
             clock_counter = excluded.clock_counter, \
             clock_device = excluded.clock_device",
            mapping.meta_table
        );

        for record in records.snapshot()? {
            if record.schema != mapping.schema || record.entity != mapping.entity {
                continue;
            }

            if record.tombstone {
                let delete_sql = format!(
                    "DELETE FROM {} WHERE {} = ?1",
                    mapping.data_table, mapping.id_column
                );
                tx.execute(&delete_sql, params![record.id])
                    .map_err(|error| Error::Protocol(error.to_string()))?;
            } else {
                let mut columns = Vec::with_capacity(1 + mapping.fields.len());
                let mut values: Vec<SqlValue> = Vec::with_capacity(1 + mapping.fields.len());
                columns.push(mapping.id_column.clone());
                values.push(SqlValue::Text(record.id.clone()));
                for field in &mapping.fields {
                    columns.push(field.column.clone());
                    let field_value = record.fields.get(&field.field).unwrap_or(&FieldValue::Null);
                    values.push(field_to_sql_value(field_value, &field.encoding));
                }

                let placeholders = (1..=columns.len())
                    .map(|idx| format!("?{idx}"))
                    .collect::<Vec<_>>();
                let update_cols = mapping
                    .fields
                    .iter()
                    .map(|field| format!("{} = excluded.{}", field.column, field.column))
                    .collect::<Vec<_>>();
                let insert_sql = if update_cols.is_empty() {
                    format!(
                        "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT({}) DO NOTHING",
                        mapping.data_table,
                        columns.join(", "),
                        placeholders.join(", "),
                        mapping.id_column,
                    )
                } else {
                    format!(
                        "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT({}) DO UPDATE SET {}",
                        mapping.data_table,
                        columns.join(", "),
                        placeholders.join(", "),
                        mapping.id_column,
                        update_cols.join(", ")
                    )
                };
                tx.execute(&insert_sql, params_from_iter(values))
                    .map_err(|error| Error::Protocol(error.to_string()))?;
            }

            let updated_at = record.updated_at.map(|value| value as i64);
            tx.execute(
                &meta_sql,
                params![
                    &mapping.schema,
                    &mapping.entity,
                    record.id,
                    if record.tombstone { 1 } else { 0 },
                    updated_at,
                    record.clock.counter as i64,
                    record.clock.device_id,
                ],
            )
            .map_err(|error| Error::Protocol(error.to_string()))?;
        }

        tx.commit()
            .map_err(|error| Error::Protocol(error.to_string()))?;
        Ok(())
    }
}

impl LogicalAdapter for SqliteLogicalAdapter {
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
            .or_else(|| {
                self.mappings
                    .iter()
                    .find_map(|mapping| mapping.policy_for(field))
            })
            .unwrap_or(MergePolicy::LastWriterWins)
    }

    fn merge_custom_field(
        &self,
        policy_name: &str,
        field: &str,
        existing: Option<&FieldValue>,
        incoming: &FieldValue,
        incoming_newer: bool,
    ) -> Option<FieldValue> {
        self.custom_merge_handlers
            .get(policy_name)
            .and_then(|handler| handler(field, existing, incoming, incoming_newer))
    }

    fn load_records(&self, records: &mut RecordState) -> Result<()> {
        if !self.mappings.is_empty() {
            for mapping in &self.mappings {
                self.load_records_mapped(records, mapping)?;
            }
            return Ok(());
        }
        let conn = self.open()?;
        self.ensure_table(&conn)?;

        let sql = format!(
            "SELECT schema, entity, id, fields, tombstone, updated_at, clock_counter, clock_device FROM {}",
            self.table
        );
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|error| Error::Protocol(error.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                let schema: String = row.get(0)?;
                let entity: String = row.get(1)?;
                let id: String = row.get(2)?;
                let fields_json: String = row.get(3)?;
                let tombstone: i64 = row.get(4)?;
                let updated_at: Option<i64> = row.get(5)?;
                let clock_counter: i64 = row.get(6)?;
                let clock_device: String = row.get(7)?;
                Ok(SyncRecord {
                    schema,
                    entity,
                    id,
                    fields: serde_json::from_str(&fields_json).unwrap_or_default(),
                    tombstone: tombstone != 0,
                    clock: LamportClock {
                        counter: clock_counter.max(0) as u64,
                        device_id: clock_device,
                    },
                    updated_at: updated_at.map(|value| value.max(0) as u64),
                })
            })
            .map_err(|error| Error::Protocol(error.to_string()))?;

        for row in rows {
            let record = row.map_err(|error| Error::Protocol(error.to_string()))?;
            records.apply(record)?;
        }
        Ok(())
    }

    fn apply_records(&self, records: &RecordView) -> Result<()> {
        if !self.mappings.is_empty() {
            for mapping in &self.mappings {
                self.apply_records_mapped(records, mapping)?;
            }
            return Ok(());
        }
        let mut conn = self.open()?;
        self.ensure_table(&conn)?;
        let tx = conn
            .transaction()
            .map_err(|error| Error::Protocol(error.to_string()))?;

        {
            let delete_sql = format!("DELETE FROM {}", self.table);
            tx.execute(&delete_sql, [])
                .map_err(|error| Error::Protocol(error.to_string()))?;

            let insert_sql = format!(
                "INSERT INTO {} (schema, entity, id, fields, tombstone, updated_at, clock_counter, clock_device)\
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                self.table
            );
            let mut stmt = tx
                .prepare(&insert_sql)
                .map_err(|error| Error::Protocol(error.to_string()))?;

            for record in records.snapshot()? {
                let fields_json = serde_json::to_string(&record.fields)?;
                stmt.execute(params![
                    record.schema,
                    record.entity,
                    record.id,
                    fields_json,
                    if record.tombstone { 1 } else { 0 },
                    record.updated_at.map(|value| value as i64),
                    record.clock.counter as i64,
                    record.clock.device_id,
                ])
                .map_err(|error| Error::Protocol(error.to_string()))?;
            }
        }

        tx.commit()
            .map_err(|error| Error::Protocol(error.to_string()))?;
        Ok(())
    }

    fn apply_snapshot(
        &self,
        records: &mut RecordState,
        incoming: Vec<SyncRecord>,
    ) -> Result<usize> {
        apply_snapshot_with_policy(
            records,
            incoming,
            |field| self.merge_policy(field),
            |policy_name, field, existing, incoming, incoming_newer| {
                self.merge_custom_field(policy_name, field, existing, incoming, incoming_newer)
            },
        )
    }
}

fn sqlite_value_to_string(value: SqlValue) -> String {
    match value {
        SqlValue::Null => "".to_string(),
        SqlValue::Integer(value) => value.to_string(),
        SqlValue::Real(value) => value.to_string(),
        SqlValue::Text(value) => value,
        SqlValue::Blob(value) => String::from_utf8_lossy(&value).to_string(),
    }
}

fn sqlite_value_to_field(value: SqlValue, encoding: &SqliteLogicalEncoding) -> FieldValue {
    match encoding {
        SqliteLogicalEncoding::Plain => match value {
            SqlValue::Null => FieldValue::Null,
            SqlValue::Integer(value) => FieldValue::I64(value),
            SqlValue::Real(value) => FieldValue::F64(value),
            SqlValue::Text(value) => FieldValue::String(value),
            SqlValue::Blob(value) => FieldValue::Bytes(value),
        },
        SqliteLogicalEncoding::Bool => match value {
            SqlValue::Null => FieldValue::Null,
            SqlValue::Integer(value) => FieldValue::Bool(value != 0),
            SqlValue::Real(value) => FieldValue::Bool(value != 0.0),
            SqlValue::Text(value) => {
                let normalized = value.to_lowercase();
                FieldValue::Bool(matches!(normalized.as_str(), "true" | "1" | "yes"))
            }
            SqlValue::Blob(value) => FieldValue::Bytes(value),
        },
        SqliteLogicalEncoding::Json => match value {
            SqlValue::Text(value) => {
                serde_json::from_str::<FieldValue>(&value).unwrap_or(FieldValue::String(value))
            }
            SqlValue::Blob(value) => {
                serde_json::from_slice::<FieldValue>(&value).unwrap_or(FieldValue::Bytes(value))
            }
            other => sqlite_value_to_field(other, &SqliteLogicalEncoding::Plain),
        },
        SqliteLogicalEncoding::JsonValue => match value {
            SqlValue::Null => FieldValue::Null,
            SqlValue::Integer(value) => FieldValue::I64(value),
            SqlValue::Real(value) => FieldValue::F64(value),
            SqlValue::Text(value) => match serde_json::from_str::<serde_json::Value>(&value) {
                Ok(value) => json_value_to_field(value),
                Err(_) => FieldValue::String(value),
            },
            SqlValue::Blob(value) => match serde_json::from_slice::<serde_json::Value>(&value) {
                Ok(value) => json_value_to_field(value),
                Err(_) => FieldValue::Bytes(value),
            },
        },
    }
}

fn field_to_sql_value(value: &FieldValue, encoding: &SqliteLogicalEncoding) -> SqlValue {
    match encoding {
        SqliteLogicalEncoding::Plain => match value {
            FieldValue::Null => SqlValue::Null,
            FieldValue::Bool(value) => SqlValue::Integer(i64::from(*value)),
            FieldValue::I64(value) => SqlValue::Integer(*value),
            FieldValue::F64(value) => SqlValue::Real(*value),
            FieldValue::String(value) => SqlValue::Text(value.clone()),
            FieldValue::Bytes(value) => SqlValue::Blob(value.clone()),
            FieldValue::List(_) | FieldValue::Map(_) => {
                let json = serde_json::to_string(value).unwrap_or_else(|_| "null".to_string());
                SqlValue::Text(json)
            }
        },
        SqliteLogicalEncoding::Bool => match value {
            FieldValue::Null => SqlValue::Null,
            FieldValue::Bool(value) => SqlValue::Integer(i64::from(*value)),
            FieldValue::I64(value) => SqlValue::Integer(*value),
            FieldValue::F64(value) => SqlValue::Integer(if *value == 0.0 { 0 } else { 1 }),
            FieldValue::String(value) => {
                let normalized = value.to_lowercase();
                let is_true = matches!(normalized.as_str(), "true" | "1" | "yes");
                SqlValue::Integer(i64::from(is_true))
            }
            FieldValue::Bytes(value) => SqlValue::Blob(value.clone()),
            FieldValue::List(_) | FieldValue::Map(_) => {
                let json = serde_json::to_string(value).unwrap_or_else(|_| "null".to_string());
                SqlValue::Text(json)
            }
        },
        SqliteLogicalEncoding::Json => match value {
            FieldValue::Null => SqlValue::Null,
            _ => {
                let json = serde_json::to_string(value).unwrap_or_else(|_| "null".to_string());
                SqlValue::Text(json)
            }
        },
        SqliteLogicalEncoding::JsonValue => match value {
            FieldValue::Null => SqlValue::Null,
            _ => {
                let json_value = field_to_json_value(value);
                let json =
                    serde_json::to_string(&json_value).unwrap_or_else(|_| "null".to_string());
                SqlValue::Text(json)
            }
        },
    }
}

fn json_value_to_field(value: serde_json::Value) -> FieldValue {
    match value {
        serde_json::Value::Null => FieldValue::Null,
        serde_json::Value::Bool(value) => FieldValue::Bool(value),
        serde_json::Value::Number(value) => {
            if let Some(int) = value.as_i64() {
                FieldValue::I64(int)
            } else if let Some(float) = value.as_f64() {
                FieldValue::F64(float)
            } else {
                FieldValue::Null
            }
        }
        serde_json::Value::String(value) => FieldValue::String(value),
        serde_json::Value::Array(items) => {
            FieldValue::List(items.into_iter().map(json_value_to_field).collect())
        }
        serde_json::Value::Object(map) => FieldValue::Map(
            map.into_iter()
                .map(|(key, value)| (key, json_value_to_field(value)))
                .collect(),
        ),
    }
}

fn field_to_json_value(value: &FieldValue) -> serde_json::Value {
    match value {
        FieldValue::Null => serde_json::Value::Null,
        FieldValue::Bool(value) => serde_json::Value::Bool(*value),
        FieldValue::I64(value) => serde_json::Value::Number((*value).into()),
        FieldValue::F64(value) => serde_json::Number::from_f64(*value)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        FieldValue::String(value) => serde_json::Value::String(value.clone()),
        FieldValue::Bytes(value) => {
            let items = value
                .iter()
                .map(|byte| serde_json::Value::Number((*byte as i64).into()))
                .collect::<Vec<_>>();
            serde_json::Value::Array(items)
        }
        FieldValue::List(items) => {
            serde_json::Value::Array(items.iter().map(field_to_json_value).collect())
        }
        FieldValue::Map(map) => serde_json::Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), field_to_json_value(value)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SqliteLogicalAdapter, SqliteLogicalEncoding, SqliteLogicalField, SqliteLogicalMapping,
    };
    use crate::{
        FieldValue, LamportClock, LogicalAdapter, MergePolicy, RecordState, RecordView, State,
        SyncRecord,
    };
    use rusqlite::Connection;
    use std::collections::BTreeMap;

    #[test]
    fn sqlite_logical_adapter_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("records.sqlite");

        let adapter = SqliteLogicalAdapter::new("logical", "app", &path, "records")
            .with_merge_policy("tags", MergePolicy::SetUnion);

        let mut state = State::new("device");
        let mut records = RecordState::new(&mut state, "app");
        records
            .set(SyncRecord {
                schema: "schema".to_string(),
                entity: "Todo".to_string(),
                id: "1".to_string(),
                fields: BTreeMap::from([(
                    "tags".to_string(),
                    FieldValue::List(vec![FieldValue::String("a".to_string())]),
                )]),
                tombstone: false,
                clock: LamportClock {
                    counter: 1,
                    device_id: "device".to_string(),
                },
                updated_at: None,
            })
            .expect("set");

        adapter
            .apply_records(&RecordView::new(&state, "app"))
            .expect("apply");

        let mut reload_state = State::new("device");
        adapter
            .load_records(&mut RecordState::new(&mut reload_state, "app"))
            .expect("load");

        let snapshot = RecordView::new(&reload_state, "app")
            .snapshot()
            .expect("snapshot");
        assert_eq!(snapshot.len(), 1);
    }

    #[test]
    fn sqlite_logical_mapping_merge_policy_precedence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("records.sqlite");

        let mapping = SqliteLogicalMapping::new("notes", "id", "schema", "Note").with_field_def(
            SqliteLogicalField::new("tags", "tags").with_merge_policy(MergePolicy::SetUnion),
        );

        let adapter =
            SqliteLogicalAdapter::new("logical", "app", &path, "records").with_mapping(mapping);

        assert_eq!(adapter.merge_policy("tags"), MergePolicy::SetUnion);
        assert_eq!(adapter.merge_policy("title"), MergePolicy::LastWriterWins);
    }

    #[test]
    fn sqlite_logical_custom_merge_handler_applies_policy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("records.sqlite");
        let adapter = SqliteLogicalAdapter::new("logical", "app", &path, "records")
            .with_merge_policy("count", MergePolicy::Custom("sum".to_string()))
            .with_custom_merge_handler(
                "sum",
                |_field, existing, incoming, _incoming_newer| match (existing, incoming) {
                    (Some(FieldValue::I64(left)), FieldValue::I64(right)) => {
                        Some(FieldValue::I64(left + right))
                    }
                    _ => None,
                },
            );

        let mut state = State::new("device");
        let mut records = RecordState::new(&mut state, "app");
        records
            .set(SyncRecord {
                schema: "schema".to_string(),
                entity: "Todo".to_string(),
                id: "1".to_string(),
                fields: BTreeMap::from([("count".to_string(), FieldValue::I64(2))]),
                tombstone: false,
                clock: LamportClock {
                    counter: 1,
                    device_id: "device".to_string(),
                },
                updated_at: None,
            })
            .expect("set");

        adapter
            .apply_snapshot(
                &mut records,
                vec![SyncRecord {
                    schema: "schema".to_string(),
                    entity: "Todo".to_string(),
                    id: "1".to_string(),
                    fields: BTreeMap::from([("count".to_string(), FieldValue::I64(3))]),
                    tombstone: false,
                    clock: LamportClock {
                        counter: 2,
                        device_id: "device".to_string(),
                    },
                    updated_at: None,
                }],
            )
            .expect("merge");

        let merged = records
            .get("schema", "Todo", "1")
            .expect("get")
            .expect("record");
        assert_eq!(merged.fields.get("count"), Some(&FieldValue::I64(5)));
    }

    #[test]
    fn sqlite_logical_json_value_encoding_round_trip() {
        let value = FieldValue::List(vec![
            FieldValue::String("alpha".to_string()),
            FieldValue::I64(42),
            FieldValue::Map(BTreeMap::from([(
                "flag".to_string(),
                FieldValue::Bool(true),
            )])),
        ]);

        let stored = super::field_to_sql_value(&value, &SqliteLogicalEncoding::JsonValue);
        let restored = super::sqlite_value_to_field(stored, &SqliteLogicalEncoding::JsonValue);
        assert_eq!(restored, value);
    }

    #[test]
    fn sqlite_logical_mapping_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mapped.sqlite");
        let conn = Connection::open(&path).expect("open");
        conn.execute(
            "CREATE TABLE todos (id TEXT PRIMARY KEY, title TEXT, done INTEGER)",
            [],
        )
        .expect("create");

        let mapping = SqliteLogicalMapping::new("todos", "id", "app", "Todo")
            .with_field("title", "title")
            .with_bool_field("done", "done");
        let adapter =
            SqliteLogicalAdapter::new("logical", "app", &path, "records").with_mapping(mapping);

        let mut state = State::new("device");
        let mut records = RecordState::new(&mut state, "app");
        records
            .set(SyncRecord {
                schema: "app".to_string(),
                entity: "Todo".to_string(),
                id: "1".to_string(),
                fields: BTreeMap::from([
                    (
                        "title".to_string(),
                        FieldValue::String("Buy milk".to_string()),
                    ),
                    ("done".to_string(), FieldValue::Bool(false)),
                ]),
                tombstone: false,
                clock: LamportClock {
                    counter: 1,
                    device_id: "device".to_string(),
                },
                updated_at: None,
            })
            .expect("set");

        adapter
            .apply_records(&RecordView::new(&state, "app"))
            .expect("apply");

        let (title, done): (String, i64) = conn
            .query_row("SELECT title, done FROM todos WHERE id = '1'", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .expect("row");
        assert_eq!(title, "Buy milk");
        assert_eq!(done, 0);

        let mut reload_state = State::new("device");
        adapter
            .load_records(&mut RecordState::new(&mut reload_state, "app"))
            .expect("load");
        let snapshot = RecordView::new(&reload_state, "app")
            .snapshot()
            .expect("snapshot");
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].id, "1");
        assert_eq!(
            snapshot[0].fields.get("done"),
            Some(&FieldValue::Bool(false))
        );
    }

    #[test]
    fn sqlite_logical_supports_multiple_table_mappings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("multi-mapped.sqlite");
        let conn = Connection::open(&path).expect("open");
        conn.execute(
            "CREATE TABLE todos (id TEXT PRIMARY KEY, title TEXT, done INTEGER)",
            [],
        )
        .expect("create todos");
        conn.execute(
            "CREATE TABLE notes (id TEXT PRIMARY KEY, body TEXT, pinned INTEGER)",
            [],
        )
        .expect("create notes");

        let todo_mapping = SqliteLogicalMapping::new("todos", "id", "app", "Todo")
            .with_field("title", "title")
            .with_bool_field("done", "done");
        let note_mapping = SqliteLogicalMapping::new("notes", "id", "app", "Note")
            .with_field("body", "body")
            .with_bool_field("pinned", "pinned");

        let adapter = SqliteLogicalAdapter::new("logical", "app", &path, "records")
            .with_mapping(todo_mapping)
            .with_mapping(note_mapping);

        let mut state = State::new("device");
        let mut records = RecordState::new(&mut state, "app");
        records
            .set(SyncRecord {
                schema: "app".to_string(),
                entity: "Todo".to_string(),
                id: "todo-1".to_string(),
                fields: BTreeMap::from([
                    (
                        "title".to_string(),
                        FieldValue::String("Buy milk".to_string()),
                    ),
                    ("done".to_string(), FieldValue::Bool(false)),
                ]),
                tombstone: false,
                clock: LamportClock {
                    counter: 1,
                    device_id: "device".to_string(),
                },
                updated_at: None,
            })
            .expect("set todo");
        records
            .set(SyncRecord {
                schema: "app".to_string(),
                entity: "Note".to_string(),
                id: "note-1".to_string(),
                fields: BTreeMap::from([
                    (
                        "body".to_string(),
                        FieldValue::String("Call mom".to_string()),
                    ),
                    ("pinned".to_string(), FieldValue::Bool(true)),
                ]),
                tombstone: false,
                clock: LamportClock {
                    counter: 2,
                    device_id: "device".to_string(),
                },
                updated_at: None,
            })
            .expect("set note");

        adapter
            .apply_records(&RecordView::new(&state, "app"))
            .expect("apply");

        let todo_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM todos", [], |row| row.get(0))
            .expect("todo count");
        let note_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM notes", [], |row| row.get(0))
            .expect("note count");
        assert_eq!(todo_count, 1);
        assert_eq!(note_count, 1);

        let mut reload_state = State::new("device");
        adapter
            .load_records(&mut RecordState::new(&mut reload_state, "app"))
            .expect("load");
        let snapshot = RecordView::new(&reload_state, "app")
            .snapshot()
            .expect("snapshot");
        assert_eq!(snapshot.len(), 2);
    }
}
