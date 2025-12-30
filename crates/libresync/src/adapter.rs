use std::fs;
use std::path::{Path, PathBuf};

use crate::{Entry, Result, State};

#[derive(Clone, Debug)]
pub enum AdapterKind {
    Document,
    Sqlite,
    Custom(String),
}

pub trait DataAdapter: Send + Sync {
    fn id(&self) -> &str;
    fn kind(&self) -> AdapterKind;

    fn load_into_state(&self, state: &mut State) -> Result<()>;
    fn apply_from_state(&self, state: &State) -> Result<()>;

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
}
