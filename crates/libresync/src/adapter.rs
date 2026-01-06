use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};

use crate::{
    Entry, Error, MergePolicy, RecordState, RecordView, Result, State, SyncRecord,
};

#[derive(Clone, Debug)]
pub enum AdapterKind {
    Document,
    Sqlite,
    Logical,
    Custom(String),
}

#[derive(Clone, Debug, Default)]
pub struct AdapterCache {
    pub last_bytes: Option<Vec<u8>>,
    pub last_wal: Option<Vec<u8>>,
    pub last_shm: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct AdapterCapabilities {
    pub logical: bool,
    pub snapshot: bool,
    pub delta: bool,
    pub watch: bool,
}

impl AdapterCapabilities {
    pub fn snapshot_only() -> Self {
        Self {
            logical: false,
            snapshot: true,
            delta: false,
            watch: false,
        }
    }

    pub fn logical_snapshot() -> Self {
        Self {
            logical: true,
            snapshot: true,
            delta: false,
            watch: false,
        }
    }
}

pub trait DataAdapter: Send + Sync {
    fn id(&self) -> &str;
    fn kind(&self) -> AdapterKind;
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities::snapshot_only()
    }

    fn load_into_state(&self, state: &mut State) -> Result<()>;
    fn apply_from_state(&self, state: &State) -> Result<()>;
    fn sync_tick(&self, state: &mut State, cache: &mut AdapterCache) -> Result<()> {
        let _ = cache;
        self.load_into_state(state)?;
        self.apply_from_state(state)?;
        Ok(())
    }

    fn export_snapshot(&self, state: &State) -> Result<Vec<Entry>> {
        Ok(state.snapshot())
    }

    fn export_delta(&self, state: &State) -> Result<Vec<Entry>> {
        Ok(state.snapshot())
    }

    fn apply_entries(&self, state: &mut State, entries: Vec<Entry>) -> Result<usize> {
        Ok(state.merge_snapshot(entries))
    }
}

pub trait LogicalAdapter: Send + Sync {
    fn id(&self) -> &str;
    fn namespace(&self) -> &str;
    fn merge_policy(&self, _field: &str) -> MergePolicy {
        MergePolicy::LastWriterWins
    }
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities::logical_snapshot()
    }

    fn load_records(&self, records: &mut RecordState) -> Result<()>;
    fn apply_records(&self, records: &RecordView) -> Result<()>;

    fn export_snapshot(&self, records: &RecordView) -> Result<Vec<SyncRecord>> {
        records.snapshot()
    }

    fn apply_snapshot(&self, records: &mut RecordState, incoming: Vec<SyncRecord>) -> Result<usize> {
        records.merge_snapshot(incoming)
    }
}

#[derive(Clone)]
pub struct LogicalAdapterWrapper {
    inner: Arc<dyn LogicalAdapter>,
}

impl LogicalAdapterWrapper {
    pub fn new(inner: Arc<dyn LogicalAdapter>) -> Self {
        Self { inner }
    }
}

impl DataAdapter for LogicalAdapterWrapper {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn kind(&self) -> AdapterKind {
        AdapterKind::Logical
    }

    fn capabilities(&self) -> AdapterCapabilities {
        self.inner.capabilities()
    }

    fn load_into_state(&self, state: &mut State) -> Result<()> {
        let mut records = RecordState::new(state, self.inner.namespace());
        self.inner.load_records(&mut records)
    }

    fn apply_from_state(&self, state: &State) -> Result<()> {
        let records = RecordView::new(state, self.inner.namespace());
        self.inner.apply_records(&records)
    }

    fn export_snapshot(&self, state: &State) -> Result<Vec<Entry>> {
        let view = RecordView::new(state, self.inner.namespace());
        let records = self.inner.export_snapshot(&view)?;
        records
            .into_iter()
            .map(|record| crate::record_to_entry(self.inner.namespace(), &record))
            .collect()
    }

    fn apply_entries(&self, state: &mut State, entries: Vec<Entry>) -> Result<usize> {
        let mut records = Vec::new();
        for entry in entries {
            if let Some(record) = crate::entry_to_record(&entry, self.inner.namespace())? {
                records.push(record);
            }
        }
        let mut record_state = RecordState::new(state, self.inner.namespace());
        self.inner.apply_snapshot(&mut record_state, records)
    }
}

#[derive(Clone, Debug)]
pub struct JsonFileAdapter {
    id: String,
    path: PathBuf,
    key: String,
}

impl JsonFileAdapter {
    pub fn new(id: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        let id = id.into();
        Self {
            key: id.clone(),
            id,
            path: path.into(),
        }
    }

    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.key = key.into();
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn ensure_file(&self) -> Result<()> {
        if self.path.exists() {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, b"{}")?;
        Ok(())
    }
}

impl DataAdapter for JsonFileAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> AdapterKind {
        AdapterKind::Document
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            logical: false,
            snapshot: true,
            delta: false,
            watch: false,
        }
    }

    fn load_into_state(&self, state: &mut State) -> Result<()> {
        self.ensure_file()?;
        let bytes = fs::read(&self.path)?;
        let current = state.get(&self.key).map(|data| data.to_vec());
        if current.as_deref() != Some(bytes.as_slice()) {
            state.set(self.key.clone(), bytes);
        }
        Ok(())
    }

    fn apply_from_state(&self, state: &State) -> Result<()> {
        if let Some(bytes) = state.get(&self.key) {
            self.ensure_file()?;
            let existing = fs::read(&self.path)?;
            if existing.as_slice() != bytes {
                fs::write(&self.path, bytes)?;
            }
        }
        Ok(())
    }

    fn sync_tick(&self, state: &mut State, cache: &mut AdapterCache) -> Result<()> {
        self.ensure_file()?;
        let bytes = state.get(&self.key).map(|data| data.to_vec());
        if let Some(bytes) = bytes {
            if cache.last_bytes.as_ref() != Some(&bytes) {
                fs::write(&self.path, &bytes)?;
                cache.last_bytes = Some(bytes);
            }
        }

        let file_bytes = fs::read(&self.path)?;
        let current = state.get(&self.key).map(|data| data.to_vec());
        if current.as_deref() != Some(file_bytes.as_slice()) {
            state.set(self.key.clone(), file_bytes);
        }

        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct SqliteFileAdapter {
    id: String,
    path: PathBuf,
    key: String,
    page_delta: Option<usize>,
}

impl SqliteFileAdapter {
    pub fn new(id: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        let id = id.into();
        Self {
            key: id.clone(),
            id,
            path: path.into(),
            page_delta: None,
        }
    }

    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.key = key.into();
        self
    }

    pub fn with_page_delta(mut self, page_size: usize) -> Self {
        self.page_delta = Some(page_size.max(512));
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn wal_path(&self) -> PathBuf {
        let path = self.path.to_string_lossy();
        PathBuf::from(format!("{path}-wal"))
    }

    fn shm_path(&self) -> PathBuf {
        let path = self.path.to_string_lossy();
        PathBuf::from(format!("{path}-shm"))
    }

    fn wal_key(&self) -> String {
        format!("{}:wal", self.key)
    }

    fn shm_key(&self) -> String {
        format!("{}:shm", self.key)
    }

    fn delta_key(&self) -> String {
        format!("{}:delta", self.key)
    }

    fn ensure_file(&self) -> Result<()> {
        if self.path.exists() {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, b"")?;
        Ok(())
    }

    fn update_state_with_sqlite_bytes(&self, state: &mut State, bytes: Vec<u8>) -> Result<()> {
        let current = state.get(&self.key).map(|data| data.to_vec());
        let delta_key = self.delta_key();
        let delta_current = state.get(&delta_key).map(|data| data.to_vec());

        if let (Some(page_size), Some(base)) = (self.page_delta, current.clone()) {
            let delta = compute_page_delta(&base, &bytes, page_size);
            if !delta.pages.is_empty() {
                let delta_bytes = bincode::serialize(&delta)
                    .map_err(|error| Error::Protocol(error.to_string()))?;
                if delta_bytes.len() < bytes.len() {
                    if delta_current.as_deref() != Some(delta_bytes.as_slice()) {
                        state.set(delta_key, delta_bytes);
                    }
                } else {
                    if current.as_deref() != Some(bytes.as_slice()) {
                        state.set(self.key.clone(), bytes);
                    }
                    if delta_current.as_deref() != Some(&[]) {
                        state.set(delta_key, Vec::new());
                    }
                }
            } else if delta_current.as_deref() != Some(&[]) {
                state.set(delta_key, Vec::new());
            }
        } else if current.as_deref() != Some(bytes.as_slice()) {
            state.set(self.key.clone(), bytes);
        }

        Ok(())
    }

    fn bytes_from_state(&self, state: &State) -> Result<Option<Vec<u8>>> {
        let base = state.get(&self.key).map(|data| data.to_vec());
        let delta_key = self.delta_key();
        if let Some(delta_bytes) = state.get(&delta_key) {
            if !delta_bytes.is_empty() {
                if let (Some(base), Ok(delta)) = (
                    base.clone(),
                    bincode::deserialize::<SqliteDelta>(delta_bytes),
                ) {
                    return Ok(Some(apply_page_delta(&base, &delta)));
                }
            }
        }
        Ok(base)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct SqliteDelta {
    page_size: u32,
    file_size: u64,
    pages: Vec<PageDelta>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct PageDelta {
    index: u32,
    bytes: Vec<u8>,
}

fn compute_page_delta(base: &[u8], current: &[u8], page_size: usize) -> SqliteDelta {
    let page_count = (current.len() + page_size - 1) / page_size;
    let mut pages = Vec::new();

    for index in 0..page_count {
        let start = index * page_size;
        let end = std::cmp::min(start + page_size, current.len());
        let current_page = &current[start..end];
        let base_page = if start < base.len() {
            let base_end = std::cmp::min(start + page_size, base.len());
            &base[start..base_end]
        } else {
            &[]
        };

        if base_page != current_page {
            pages.push(PageDelta {
                index: index as u32,
                bytes: current_page.to_vec(),
            });
        }
    }

    SqliteDelta {
        page_size: page_size as u32,
        file_size: current.len() as u64,
        pages,
    }
}

fn apply_page_delta(base: &[u8], delta: &SqliteDelta) -> Vec<u8> {
    let page_size = delta.page_size as usize;
    let mut output = base.to_vec();
    output.resize(delta.file_size as usize, 0);

    for page in &delta.pages {
        let start = page.index as usize * page_size;
        let end = start + page.bytes.len();
        if end > output.len() {
            output.resize(end, 0);
        }
        output[start..end].copy_from_slice(&page.bytes);
    }

    output
}

impl DataAdapter for SqliteFileAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> AdapterKind {
        AdapterKind::Sqlite
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            logical: false,
            snapshot: true,
            delta: true,
            watch: false,
        }
    }

    fn load_into_state(&self, state: &mut State) -> Result<()> {
        self.ensure_file()?;
        let bytes = fs::read(&self.path)?;
        self.update_state_with_sqlite_bytes(state, bytes)?;

        let wal_path = self.wal_path();
        let wal_bytes = if wal_path.exists() {
            fs::read(&wal_path)?
        } else {
            Vec::new()
        };
        let wal_key = self.wal_key();
        let current_wal = state.get(&wal_key).map(|data| data.to_vec());
        if current_wal.as_deref() != Some(wal_bytes.as_slice()) {
            state.set(wal_key, wal_bytes);
        }

        let shm_path = self.shm_path();
        let shm_bytes = if shm_path.exists() {
            fs::read(&shm_path)?
        } else {
            Vec::new()
        };
        let shm_key = self.shm_key();
        let current_shm = state.get(&shm_key).map(|data| data.to_vec());
        if current_shm.as_deref() != Some(shm_bytes.as_slice()) {
            state.set(shm_key, shm_bytes);
        }
        Ok(())
    }

    fn apply_from_state(&self, state: &State) -> Result<()> {
        if let Some(bytes) = self.bytes_from_state(state)? {
            self.ensure_file()?;
            let existing = fs::read(&self.path)?;
            if existing.as_slice() != bytes {
                fs::write(&self.path, bytes)?;
            }
        }

        let wal_key = self.wal_key();
        if let Some(bytes) = state.get(&wal_key) {
            let wal_path = self.wal_path();
            if bytes.is_empty() {
                let _ = fs::remove_file(&wal_path);
            } else {
                fs::write(&wal_path, bytes)?;
            }
        }

        let shm_key = self.shm_key();
        if let Some(bytes) = state.get(&shm_key) {
            let shm_path = self.shm_path();
            if bytes.is_empty() {
                let _ = fs::remove_file(&shm_path);
            } else {
                fs::write(&shm_path, bytes)?;
            }
        }
        Ok(())
    }

    fn sync_tick(&self, state: &mut State, cache: &mut AdapterCache) -> Result<()> {
        self.ensure_file()?;
        if let Some(bytes) = self.bytes_from_state(state)? {
            if cache.last_bytes.as_ref() != Some(&bytes) {
                fs::write(&self.path, &bytes)?;
                cache.last_bytes = Some(bytes);
            }
        }

        let wal_key = self.wal_key();
        if let Some(bytes) = state.get(&wal_key).map(|data| data.to_vec()) {
            if cache.last_wal.as_ref() != Some(&bytes) {
                let wal_path = self.wal_path();
                if bytes.is_empty() {
                    let _ = fs::remove_file(&wal_path);
                } else {
                    fs::write(&wal_path, &bytes)?;
                }
                cache.last_wal = Some(bytes);
            }
        }

        let shm_key = self.shm_key();
        if let Some(bytes) = state.get(&shm_key).map(|data| data.to_vec()) {
            if cache.last_shm.as_ref() != Some(&bytes) {
                let shm_path = self.shm_path();
                if bytes.is_empty() {
                    let _ = fs::remove_file(&shm_path);
                } else {
                    fs::write(&shm_path, &bytes)?;
                }
                cache.last_shm = Some(bytes);
            }
        }

        let file_bytes = fs::read(&self.path)?;
        self.update_state_with_sqlite_bytes(state, file_bytes)?;

        let wal_path = self.wal_path();
        let wal_bytes = if wal_path.exists() {
            fs::read(&wal_path)?
        } else {
            Vec::new()
        };
        let current_wal = state.get(&wal_key).map(|data| data.to_vec());
        if current_wal.as_deref() != Some(wal_bytes.as_slice()) {
            state.set(wal_key, wal_bytes.clone());
        }
        cache.last_wal = Some(wal_bytes);

        let shm_path = self.shm_path();
        let shm_bytes = if shm_path.exists() {
            fs::read(&shm_path)?
        } else {
            Vec::new()
        };
        let current_shm = state.get(&shm_key).map(|data| data.to_vec());
        if current_shm.as_deref() != Some(shm_bytes.as_slice()) {
            state.set(shm_key, shm_bytes.clone());
        }
        cache.last_shm = Some(shm_bytes);

        Ok(())
    }
}

pub struct WatchedFileAdapter {
    id: String,
    path: PathBuf,
    key: String,
    default_bytes: Vec<u8>,
    #[allow(dead_code)]
    watcher: Mutex<RecommendedWatcher>,
    receiver: Mutex<mpsc::Receiver<notify::Result<notify::Event>>>,
}

impl WatchedFileAdapter {
    pub fn new(id: impl Into<String>, path: impl Into<PathBuf>) -> Result<Self> {
        Self::new_with_default(id, path, Vec::new())
    }

    pub fn new_json(id: impl Into<String>, path: impl Into<PathBuf>) -> Result<Self> {
        Self::new_with_default(id, path, b"{}".to_vec())
    }

    pub fn new_with_default(
        id: impl Into<String>,
        path: impl Into<PathBuf>,
        default_bytes: Vec<u8>,
    ) -> Result<Self> {
        let id = id.into();
        let path = path.into();
        ensure_file_with_default(&path, &default_bytes)?;

        let (tx, rx) = mpsc::channel();
        let mut watcher =
            RecommendedWatcher::new(tx, notify::Config::default())
                .map_err(|error| Error::Protocol(error.to_string()))?;
        watcher
            .watch(&path, RecursiveMode::NonRecursive)
            .map_err(|error| Error::Protocol(error.to_string()))?;

        Ok(Self {
            key: id.clone(),
            id,
            path,
            default_bytes,
            watcher: Mutex::new(watcher),
            receiver: Mutex::new(rx),
        })
    }

    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.key = key.into();
        self
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn ensure_file(&self) -> Result<()> {
        ensure_file_with_default(&self.path, &self.default_bytes)
    }

    fn drain_events(&self) -> Result<bool> {
        let mut changed = false;
        let receiver = self
            .receiver
            .lock()
            .map_err(|_| Error::Protocol("watch receiver lock poisoned".to_string()))?;
        loop {
            match receiver.try_recv() {
                Ok(Ok(_)) => changed = true,
                Ok(Err(error)) => return Err(Error::Protocol(error.to_string())),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
        Ok(changed)
    }
}

impl DataAdapter for WatchedFileAdapter {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> AdapterKind {
        AdapterKind::Document
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            logical: false,
            snapshot: true,
            delta: false,
            watch: true,
        }
    }

    fn load_into_state(&self, state: &mut State) -> Result<()> {
        self.ensure_file()?;
        let bytes = fs::read(&self.path)?;
        let current = state.get(&self.key).map(|data| data.to_vec());
        if current.as_deref() != Some(bytes.as_slice()) {
            state.set(self.key.clone(), bytes);
        }
        Ok(())
    }

    fn apply_from_state(&self, state: &State) -> Result<()> {
        if let Some(bytes) = state.get(&self.key) {
            self.ensure_file()?;
            let existing = fs::read(&self.path)?;
            if existing.as_slice() != bytes {
                fs::write(&self.path, bytes)?;
            }
        }
        Ok(())
    }

    fn sync_tick(&self, state: &mut State, cache: &mut AdapterCache) -> Result<()> {
        self.ensure_file()?;

        let bytes = state.get(&self.key).map(|data| data.to_vec());
        if let Some(bytes) = bytes {
            if cache.last_bytes.as_ref() != Some(&bytes) {
                fs::write(&self.path, &bytes)?;
                cache.last_bytes = Some(bytes);
            }
        }

        let mut read_file = cache.last_bytes.is_none();
        if self.drain_events()? {
            read_file = true;
        }

        if read_file {
            let file_bytes = fs::read(&self.path)?;
            let current = state.get(&self.key).map(|data| data.to_vec());
            if current.as_deref() != Some(file_bytes.as_slice()) {
                state.set(self.key.clone(), file_bytes.clone());
            }
            cache.last_bytes = Some(file_bytes);
        }

        Ok(())
    }
}

fn ensure_file_with_default(path: &Path, default_bytes: &[u8]) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, default_bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AdapterCache, DataAdapter, JsonFileAdapter, SqliteFileAdapter, WatchedFileAdapter,
    };
    use crate::State;

    #[test]
    fn sqlite_adapter_loads_wal_and_shm() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("db.sqlite");
        let wal_path = dir.path().join("db.sqlite-wal");
        let shm_path = dir.path().join("db.sqlite-shm");

        std::fs::write(&db_path, b"db").expect("write db");
        std::fs::write(&wal_path, b"wal").expect("write wal");
        std::fs::write(&shm_path, b"shm").expect("write shm");

        let adapter = SqliteFileAdapter::new("db", &db_path);
        let mut state = State::new("device");
        adapter.load_into_state(&mut state).expect("load");

        assert_eq!(state.get("db"), Some(b"db".as_slice()));
        assert_eq!(state.get("db:wal"), Some(b"wal".as_slice()));
        assert_eq!(state.get("db:shm"), Some(b"shm".as_slice()));

        let mut cache = AdapterCache::default();
        adapter.sync_tick(&mut state, &mut cache).expect("sync");
    }

    #[test]
    fn json_adapter_sync_tick_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("data.json");
        std::fs::write(&path, b"{\"a\":1}").expect("write json");

        let adapter = JsonFileAdapter::new("file", &path);
        let mut state = State::new("device");
        let mut cache = AdapterCache::default();

        adapter.sync_tick(&mut state, &mut cache).expect("sync");
        assert_eq!(state.get("file"), Some(b"{\"a\":1}".as_slice()));

        state.set("file", b"{\"b\":2}".to_vec());
        adapter.sync_tick(&mut state, &mut cache).expect("sync 2");
        let updated = std::fs::read(&path).expect("read");
        assert_eq!(updated, b"{\"b\":2}");
    }

    #[test]
    fn sqlite_adapter_apply_removes_empty_wal_and_shm() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("db.sqlite");
        let wal_path = dir.path().join("db.sqlite-wal");
        let shm_path = dir.path().join("db.sqlite-shm");

        std::fs::write(&db_path, b"db").expect("write db");
        std::fs::write(&wal_path, b"wal").expect("write wal");
        std::fs::write(&shm_path, b"shm").expect("write shm");

        let adapter = SqliteFileAdapter::new("db", &db_path);
        let mut state = State::new("device");
        state.set("db", b"db".to_vec());
        state.set("db:wal", Vec::new());
        state.set("db:shm", Vec::new());

        adapter.apply_from_state(&state).expect("apply");
        assert!(db_path.exists());
        assert!(!wal_path.exists());
        assert!(!shm_path.exists());
    }

    #[test]
    fn watched_file_adapter_reads_file_on_sync_tick() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("data.json");
        std::fs::write(&path, b"{\"a\":1}").expect("write json");

        let adapter = WatchedFileAdapter::new_json("watch", &path).expect("adapter");
        let mut state = State::new("device");
        let mut cache = AdapterCache::default();

        adapter.sync_tick(&mut state, &mut cache).expect("sync");
        assert_eq!(state.get("watch"), Some(b"{\"a\":1}".as_slice()));
    }

    #[test]
    fn sqlite_adapter_creates_page_delta_when_smaller() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("db.sqlite");

        let base = vec![0u8; 4096];
        let mut current = base.clone();
        current[10] = 42;
        std::fs::write(&db_path, &current).expect("write db");

        let adapter = SqliteFileAdapter::new("db", &db_path).with_page_delta(1024);
        let mut state = State::new("device");
        state.set("db", base);

        adapter.load_into_state(&mut state).expect("load");

        let delta = state.get("db:delta").expect("delta");
        let decoded: super::SqliteDelta =
            bincode::deserialize(delta).expect("decode delta");
        assert!(!decoded.pages.is_empty());
    }

    #[test]
    fn sqlite_adapter_apply_page_delta_updates_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("db.sqlite");

        let base = vec![0u8; 2048];
        let mut current = base.clone();
        current[100] = 7;
        let delta = super::compute_page_delta(&base, &current, 512);
        let delta_bytes = bincode::serialize(&delta).expect("serialize");

        let adapter = SqliteFileAdapter::new("db", &db_path).with_page_delta(512);
        let mut state = State::new("device");
        state.set("db", base);
        state.set("db:delta", delta_bytes);

        adapter.apply_from_state(&state).expect("apply");
        let on_disk = std::fs::read(&db_path).expect("read");
        assert_eq!(on_disk, current);
    }
}
