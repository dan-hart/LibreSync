use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};

use crate::{
    decrypt_entries, encrypt_entries, parse_record_key, AppKey, DataAdapter, Entry, Error,
    LamportClock, Result, State,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotMetadata {
    pub id: String,
    pub adapter_id: String,
    pub created_at_unix_secs: u64,
    pub entry_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by_device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Snapshot {
    pub metadata: SnapshotMetadata,
    pub entries: Vec<Entry>,
}

pub trait SnapshotStore: Send + Sync {
    fn save_snapshot(&self, snapshot: &Snapshot) -> Result<()>;
    fn load_snapshot(&self, adapter_id: &str, snapshot_id: &str) -> Result<Snapshot>;
    fn list_snapshots(&self, adapter_id: &str) -> Result<Vec<SnapshotMetadata>>;
    fn delete_snapshot(&self, adapter_id: &str, snapshot_id: &str) -> Result<()>;
}

#[derive(Clone, Debug)]
pub struct FileSnapshotStore {
    root: PathBuf,
}

impl FileSnapshotStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn adapter_dir(&self, adapter_id: &str) -> PathBuf {
        self.root.join(adapter_id)
    }

    fn snapshot_path(&self, adapter_id: &str, snapshot_id: &str) -> PathBuf {
        self.adapter_dir(adapter_id).join(format!("{snapshot_id}.json"))
    }
}

impl SnapshotStore for FileSnapshotStore {
    fn save_snapshot(&self, snapshot: &Snapshot) -> Result<()> {
        let dir = self.adapter_dir(&snapshot.metadata.adapter_id);
        fs::create_dir_all(&dir)?;
        let path = self.snapshot_path(&snapshot.metadata.adapter_id, &snapshot.metadata.id);
        let data = serde_json::to_vec_pretty(snapshot)?;
        fs::write(path, data)?;
        Ok(())
    }

    fn load_snapshot(&self, adapter_id: &str, snapshot_id: &str) -> Result<Snapshot> {
        let path = self.snapshot_path(adapter_id, snapshot_id);
        let data = fs::read(path)?;
        let snapshot = serde_json::from_slice(&data)?;
        Ok(snapshot)
    }

    fn list_snapshots(&self, adapter_id: &str) -> Result<Vec<SnapshotMetadata>> {
        let dir = self.adapter_dir(adapter_id);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut snapshots = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let data = fs::read(entry.path())?;
            let snapshot: Snapshot = serde_json::from_slice(&data)?;
            snapshots.push(snapshot.metadata);
        }
        snapshots.sort_by(|a, b| a.created_at_unix_secs.cmp(&b.created_at_unix_secs));
        Ok(snapshots)
    }

    fn delete_snapshot(&self, adapter_id: &str, snapshot_id: &str) -> Result<()> {
        let path = self.snapshot_path(adapter_id, snapshot_id);
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

pub trait BackupAdapter: Send + Sync {
    fn id(&self) -> &str;
    fn export_snapshot(&self, state: &State) -> Result<Vec<Entry>>;
    fn apply_snapshot(&self, state: &mut State, entries: Vec<Entry>) -> Result<usize>;
}

pub struct DataAdapterBackup {
    adapter: std::sync::Arc<dyn DataAdapter>,
}

impl DataAdapterBackup {
    pub fn new(adapter: std::sync::Arc<dyn DataAdapter>) -> Self {
        Self { adapter }
    }
}

impl BackupAdapter for DataAdapterBackup {
    fn id(&self) -> &str {
        self.adapter.id()
    }

    fn export_snapshot(&self, state: &State) -> Result<Vec<Entry>> {
        self.adapter.export_snapshot(state)
    }

    fn apply_snapshot(&self, state: &mut State, entries: Vec<Entry>) -> Result<usize> {
        self.adapter.apply_entries(state, entries)
    }
}

#[derive(Clone, Debug, Default)]
pub struct RestoreOptions {
    pub confirm: bool,
}

impl RestoreOptions {
    pub fn confirmed() -> Self {
        Self { confirm: true }
    }
}

pub struct BackupManager {
    app_key: AppKey,
    store: std::sync::Arc<dyn SnapshotStore>,
}

#[derive(Clone, Debug, Default)]
pub struct RetentionPolicy {
    pub max_snapshots: Option<usize>,
    pub max_age_secs: Option<u64>,
}

impl RetentionPolicy {
    pub fn is_empty(&self) -> bool {
        self.max_snapshots.is_none() && self.max_age_secs.is_none()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrunePlan {
    pub adapter_id: String,
    pub total: usize,
    pub to_delete: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PruneSummary {
    pub adapter_id: String,
    pub deleted: Vec<String>,
    pub remaining: usize,
}

impl BackupManager {
    pub fn new(app_key: AppKey, store: std::sync::Arc<dyn SnapshotStore>) -> Self {
        Self { app_key, store }
    }

    pub fn create_snapshot(
        &self,
        adapter: &dyn BackupAdapter,
        state: &State,
        note: Option<String>,
    ) -> Result<SnapshotMetadata> {
        let entries = adapter.export_snapshot(state)?;
        let encrypted = encrypt_entries(&self.app_key, entries)?;
        let label = note.clone();
        let metadata = SnapshotMetadata {
            id: new_snapshot_id(),
            adapter_id: adapter.id().to_string(),
            created_at_unix_secs: now_unix_secs(),
            entry_count: encrypted.len(),
            created_by_device_id: Some(state.device_id.clone()),
            label,
            note,
        };
        let snapshot = Snapshot {
            metadata: metadata.clone(),
            entries: encrypted,
        };
        self.store.save_snapshot(&snapshot)?;
        Ok(metadata)
    }

    pub fn list_snapshots(&self, adapter_id: &str) -> Result<Vec<SnapshotMetadata>> {
        self.store.list_snapshots(adapter_id)
    }

    pub fn load_snapshot_entries(
        &self,
        adapter_id: &str,
        snapshot_id: &str,
    ) -> Result<(SnapshotMetadata, Vec<Entry>)> {
        let snapshot = self.store.load_snapshot(adapter_id, snapshot_id)?;
        let entries = decrypt_entries(&self.app_key, snapshot.entries)?;
        Ok((snapshot.metadata, entries))
    }

    pub fn restore_snapshot(
        &self,
        adapter: &dyn BackupAdapter,
        state: &mut State,
        snapshot_id: &str,
        options: RestoreOptions,
    ) -> Result<SnapshotMetadata> {
        if !options.confirm {
            return Err(Error::Protocol(
                "restore requires explicit confirmation".to_string(),
            ));
        }

        let snapshot = self.store.load_snapshot(adapter.id(), snapshot_id)?;
        let entries = decrypt_entries(&self.app_key, snapshot.entries)?;
        let entries = reclock_entries_for_restore(state, entries);
        adapter.apply_snapshot(state, entries)?;
        Ok(snapshot.metadata)
    }

    pub fn plan_prune(
        &self,
        adapter_id: &str,
        policy: RetentionPolicy,
    ) -> Result<PrunePlan> {
        let mut snapshots = self.store.list_snapshots(adapter_id)?;
        snapshots.sort_by(|a, b| a.created_at_unix_secs.cmp(&b.created_at_unix_secs));

        let total = snapshots.len();
        if total == 0 || policy.is_empty() {
            return Ok(PrunePlan {
                adapter_id: adapter_id.to_string(),
                total,
                to_delete: Vec::new(),
            });
        }

        let mut to_delete = HashSet::new();

        if let Some(max_age) = policy.max_age_secs {
            let cutoff = now_unix_secs().saturating_sub(max_age);
            for snapshot in snapshots.iter() {
                if snapshot.created_at_unix_secs < cutoff {
                    to_delete.insert(snapshot.id.clone());
                }
            }
        }

        if let Some(max_keep) = policy.max_snapshots {
            if total > max_keep {
                let mut remaining = total.saturating_sub(to_delete.len());
                for snapshot in snapshots.iter() {
                    if remaining <= max_keep {
                        break;
                    }
                    if to_delete.insert(snapshot.id.clone()) {
                        remaining = remaining.saturating_sub(1);
                    }
                }
            }
        }

        let mut to_delete = to_delete.into_iter().collect::<Vec<_>>();
        to_delete.sort();

        Ok(PrunePlan {
            adapter_id: adapter_id.to_string(),
            total,
            to_delete,
        })
    }

    pub fn prune_snapshots(
        &self,
        adapter_id: &str,
        policy: RetentionPolicy,
    ) -> Result<PruneSummary> {
        let plan = self.plan_prune(adapter_id, policy)?;
        for snapshot_id in &plan.to_delete {
            self.store.delete_snapshot(adapter_id, snapshot_id)?;
        }
        let remaining = plan.total.saturating_sub(plan.to_delete.len());
        Ok(PruneSummary {
            adapter_id: adapter_id.to_string(),
            deleted: plan.to_delete,
            remaining,
        })
    }

    pub fn reencrypt_snapshots(
        &self,
        adapter_id: &str,
        new_key: &AppKey,
    ) -> Result<usize> {
        let snapshots = self.store.list_snapshots(adapter_id)?;
        let mut updated = 0usize;
        for metadata in snapshots {
            let snapshot = self.store.load_snapshot(adapter_id, &metadata.id)?;
            let entries = decrypt_entries(&self.app_key, snapshot.entries)?;
            let encrypted = encrypt_entries(new_key, entries)?;
            let updated_snapshot = Snapshot {
                metadata: snapshot.metadata,
                entries: encrypted,
            };
            self.store.save_snapshot(&updated_snapshot)?;
            updated += 1;
        }
        Ok(updated)
    }
}

fn reclock_entries_for_restore(state: &mut State, entries: Vec<Entry>) -> Vec<Entry> {
    entries
        .into_iter()
        .map(|entry| {
            state.counter = state.counter.saturating_add(1);
            Entry {
                key: entry.key,
                value: entry.value,
                clock: LamportClock {
                    counter: state.counter,
                    device_id: state.device_id.clone(),
                },
            }
        })
        .collect()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffCounts {
    pub new_entries: usize,
    pub changed_entries: usize,
    pub unchanged_entries: usize,
    pub missing_entries: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotDiffSummary {
    pub total_snapshot_entries: usize,
    pub total_state_entries: usize,
    pub overall: DiffCounts,
    pub by_group: BTreeMap<String, DiffCounts>,
}

pub fn summarize_snapshot_diff(state: &State, snapshot_entries: &[Entry]) -> Result<SnapshotDiffSummary> {
    let mut snapshot_map: HashMap<&str, &Entry> = HashMap::new();
    for entry in snapshot_entries {
        snapshot_map.insert(entry.key.as_str(), entry);
    }

    let mut summary = SnapshotDiffSummary {
        total_snapshot_entries: snapshot_entries.len(),
        total_state_entries: state.entries.len(),
        overall: DiffCounts::default(),
        by_group: BTreeMap::new(),
    };

    for entry in snapshot_entries {
        let group = diff_group(&entry.key)?;
        let counts = summary.by_group.entry(group).or_default();

        match state.entries.get(&entry.key) {
            None => {
                counts.new_entries += 1;
                summary.overall.new_entries += 1;
            }
            Some(existing) if existing.value == entry.value => {
                counts.unchanged_entries += 1;
                summary.overall.unchanged_entries += 1;
            }
            Some(_) => {
                counts.changed_entries += 1;
                summary.overall.changed_entries += 1;
            }
        }
    }

    for (key, entry) in &state.entries {
        if snapshot_map.contains_key(key.as_str()) {
            continue;
        }
        let group = diff_group(key)?;
        let counts = summary.by_group.entry(group).or_default();
        counts.missing_entries += 1;
        summary.overall.missing_entries += 1;
        let _ = entry;
    }

    Ok(summary)
}

fn new_snapshot_id() -> String {
    let mut rand_bytes = [0u8; 8];
    OsRng.fill_bytes(&mut rand_bytes);
    let mut hex = String::with_capacity(16);
    for byte in rand_bytes {
        hex.push(hex_char(byte >> 4));
        hex.push(hex_char(byte & 0x0f));
    }
    format!("{}-{}", now_unix_secs(), hex)
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn hex_char(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        _ => (b'a' + (value - 10)) as char,
    }
}

fn diff_group(key: &str) -> Result<String> {
    if let Some(parts) = parse_record_key(key)? {
        return Ok(format!("{}/{}", parts.schema, parts.entity));
    }
    Ok(key.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn file_snapshot_store_round_trip() {
        let temp = tempdir().expect("tempdir");
        let store = FileSnapshotStore::new(temp.path()).expect("store");
        let app_key = AppKey::generate().expect("app key");
        let manager = BackupManager::new(app_key, std::sync::Arc::new(store));

        let adapter = crate::JsonFileAdapter::new("file", temp.path().join("data.json"));
        let adapter = DataAdapterBackup::new(std::sync::Arc::new(adapter));

        let mut state = State::new("device");
        state.set("file", b"{}".to_vec());

        let metadata = manager
            .create_snapshot(&adapter, &state, Some("initial".to_string()))
            .expect("snapshot");
        let listed = manager
            .list_snapshots(adapter.id())
            .expect("list snapshots");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, metadata.id);

        let mut restore_state = State::new("device");
        manager
            .restore_snapshot(&adapter, &mut restore_state, &metadata.id, RestoreOptions::confirmed())
            .expect("restore");
        assert_eq!(restore_state.get("file"), Some(b"{}".as_slice()));
    }

    #[test]
    fn summarize_snapshot_diff_counts_entries() {
        let mut state = State::new("device");
        state.set("alpha", b"one".to_vec());
        state.set("bravo", b"two".to_vec());

        let snapshot_entries = vec![Entry {
            key: "alpha".to_string(),
            value: b"one".to_vec(),
            clock: state.entries.get("alpha").expect("alpha").clock.clone(),
        }];

        let summary = summarize_snapshot_diff(&state, &snapshot_entries).expect("summary");
        assert_eq!(summary.overall.unchanged_entries, 1);
        assert_eq!(summary.overall.missing_entries, 1);
    }

    #[test]
    fn restore_requires_confirmation() {
        let temp = tempdir().expect("tempdir");
        let store = FileSnapshotStore::new(temp.path()).expect("store");
        let app_key = AppKey::generate().expect("app key");
        let manager = BackupManager::new(app_key, std::sync::Arc::new(store));

        let adapter = crate::JsonFileAdapter::new("file", temp.path().join("data.json"));
        let adapter = DataAdapterBackup::new(std::sync::Arc::new(adapter));

        let mut state = State::new("device");
        state.set("file", b"{}".to_vec());

        let metadata = manager
            .create_snapshot(&adapter, &state, None)
            .expect("snapshot");

        let result = manager.restore_snapshot(
            &adapter,
            &mut state,
            &metadata.id,
            RestoreOptions::default(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn restore_snapshot_overwrites_state() {
        let temp = tempdir().expect("tempdir");
        let store = FileSnapshotStore::new(temp.path()).expect("store");
        let app_key = AppKey::generate().expect("app key");
        let manager = BackupManager::new(app_key, std::sync::Arc::new(store));

        let adapter = crate::JsonFileAdapter::new("file", temp.path().join("data.json"));
        let adapter = DataAdapterBackup::new(std::sync::Arc::new(adapter));

        let mut state = State::new("device");
        state.set("file", b"{\"before\":true}".to_vec());
        let metadata = manager
            .create_snapshot(&adapter, &state, None)
            .expect("snapshot");

        state.set("file", b"{\"after\":true}".to_vec());

        manager
            .restore_snapshot(&adapter, &mut state, &metadata.id, RestoreOptions::confirmed())
            .expect("restore");

        assert_eq!(state.get("file"), Some(b"{\"before\":true}".as_slice()));
    }

    #[test]
    fn summarize_diff_groups_record_keys() {
        let mut state = State::new("device");
        let key = crate::record_entry_key("app", "schema/1", "Todo", "1");
        state.set(&key, b"one".to_vec());

        let snapshot_entries = vec![Entry {
            key: key.clone(),
            value: b"one".to_vec(),
            clock: state.entries.get(&key).expect("entry").clock.clone(),
        }];

        let summary = summarize_snapshot_diff(&state, &snapshot_entries).expect("summary");
        assert!(summary.by_group.contains_key("schema/1/Todo"));
    }

    #[test]
    fn prune_plan_respects_age_and_count() {
        let temp = tempdir().expect("tempdir");
        let store = FileSnapshotStore::new(temp.path()).expect("store");
        let app_key = AppKey::generate().expect("app key");
        let manager = BackupManager::new(app_key, std::sync::Arc::new(store.clone()));

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        for (id, created_at) in [
            ("snap-1", now.saturating_sub(1200)),
            ("snap-2", now.saturating_sub(400)),
            ("snap-3", now.saturating_sub(10)),
        ] {
            let metadata = SnapshotMetadata {
                id: id.to_string(),
                adapter_id: "file".to_string(),
                created_at_unix_secs: created_at,
                entry_count: 0,
                created_by_device_id: None,
                label: None,
                note: None,
            };
            let snapshot = Snapshot {
                metadata,
                entries: Vec::new(),
            };
            store.save_snapshot(&snapshot).expect("save snapshot");
        }

        let plan = manager
            .plan_prune(
                "file",
                RetentionPolicy {
                    max_snapshots: Some(2),
                    max_age_secs: Some(600),
                },
            )
            .expect("plan");

        assert_eq!(plan.total, 3);
        assert_eq!(plan.to_delete.len(), 1);
        assert!(plan.to_delete.contains(&"snap-1".to_string()));
    }

    #[test]
    fn prune_snapshots_deletes_oldest() {
        let temp = tempdir().expect("tempdir");
        let store = FileSnapshotStore::new(temp.path()).expect("store");
        let app_key = AppKey::generate().expect("app key");
        let manager = BackupManager::new(app_key, std::sync::Arc::new(store.clone()));

        for id in ["snap-a", "snap-b", "snap-c"] {
            let metadata = SnapshotMetadata {
                id: id.to_string(),
                adapter_id: "file".to_string(),
                created_at_unix_secs: now_unix_secs(),
                entry_count: 0,
                created_by_device_id: None,
                label: None,
                note: None,
            };
            let snapshot = Snapshot {
                metadata,
                entries: Vec::new(),
            };
            store.save_snapshot(&snapshot).expect("save snapshot");
        }

        let summary = manager
            .prune_snapshots(
                "file",
                RetentionPolicy {
                    max_snapshots: Some(2),
                    max_age_secs: None,
                },
            )
            .expect("prune");

        assert_eq!(summary.deleted.len(), 1);
        let remaining = manager.list_snapshots("file").expect("list");
        assert_eq!(remaining.len(), 2);
    }

    #[test]
    fn reencrypt_snapshots_rotates_keys() {
        let temp = tempdir().expect("tempdir");
        let store = FileSnapshotStore::new(temp.path()).expect("store");
        let old_key = AppKey::generate().expect("app key");
        let new_key = AppKey::generate().expect("app key");
        let manager = BackupManager::new(old_key.clone(), std::sync::Arc::new(store.clone()));

        let adapter = crate::JsonFileAdapter::new("file", temp.path().join("data.json"));
        let adapter = DataAdapterBackup::new(std::sync::Arc::new(adapter));
        let mut state = State::new("device");
        state.set("file", b"{\"alpha\":1}".to_vec());
        let metadata = manager
            .create_snapshot(&adapter, &state, None)
            .expect("snapshot");

        let rotated = manager
            .reencrypt_snapshots(adapter.id(), &new_key)
            .expect("reencrypt");
        assert_eq!(rotated, 1);

        let manager_new = BackupManager::new(new_key, std::sync::Arc::new(store));
        let mut restore_state = State::new("device");
        manager_new
            .restore_snapshot(&adapter, &mut restore_state, &metadata.id, RestoreOptions::confirmed())
            .expect("restore");
        assert_eq!(restore_state.get("file"), Some(b"{\"alpha\":1}".as_slice()));
    }
}
