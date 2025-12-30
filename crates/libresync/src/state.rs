use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Entry, LamportClock, Result};

#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct State {
    pub device_id: String,
    pub counter: u64,
    pub entries: BTreeMap<String, Entry>,
}

impl State {
    pub fn new(device_id: impl Into<String>) -> Self {
        Self {
            device_id: device_id.into(),
            counter: 0,
            entries: BTreeMap::new(),
        }
    }

    pub fn set(&mut self, key: impl Into<String>, value: Vec<u8>) -> Entry {
        self.counter = self.counter.saturating_add(1);
        let key = key.into();
        let entry = Entry {
            key: key.clone(),
            value,
            clock: LamportClock {
                counter: self.counter,
                device_id: self.device_id.clone(),
            },
        };
        self.entries.insert(key, entry.clone());
        entry
    }

    pub fn get(&self, key: &str) -> Option<&[u8]> {
        self.entries.get(key).map(|entry| entry.value.as_slice())
    }

    pub fn apply_entry(&mut self, entry: Entry) -> bool {
        if entry.clock.counter > self.counter {
            self.counter = entry.clock.counter;
        }
        match self.entries.get(&entry.key) {
            Some(existing) if existing.clock >= entry.clock => false,
            _ => {
                self.entries.insert(entry.key.clone(), entry);
                true
            }
        }
    }

    pub fn merge_snapshot(&mut self, entries: impl IntoIterator<Item = Entry>) -> usize {
        let mut applied = 0;
        for entry in entries {
            if self.apply_entry(entry) {
                applied += 1;
            }
        }
        applied
    }

    pub fn snapshot(&self) -> Vec<Entry> {
        self.entries.values().cloned().collect()
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let serialized = serde_json::to_vec_pretty(self)?;
        fs::write(path, serialized)?;
        Ok(())
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let data = fs::read(path)?;
        let state = serde_json::from_slice(&data)?;
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::State;
    use crate::LamportClock;
    use std::path::PathBuf;

    #[test]
    fn state_set_get_updates_counter() {
        let mut state = State::new("device-a");
        state.set("alpha", b"one".to_vec());
        assert_eq!(state.counter, 1);
        assert_eq!(state.get("alpha"), Some("one".as_bytes()));
    }

    #[test]
    fn state_merge_snapshot_prefers_newer_clock() {
        let mut state = State::new("device-a");
        state.set("alpha", b"one".to_vec());

        let newer_entry = crate::Entry {
            key: "alpha".to_string(),
            value: b"two".to_vec(),
            clock: LamportClock {
                counter: 2,
                device_id: "device-b".to_string(),
            },
        };

        let applied = state.apply_entry(newer_entry);
        assert!(applied);
        assert_eq!(state.get("alpha"), Some("two".as_bytes()));
    }

    #[test]
    fn state_merge_snapshot_resolves_ties_by_device() {
        let mut state = State::new("device-a");
        state.set("alpha", b"one".to_vec());

        let tied_entry = crate::Entry {
            key: "alpha".to_string(),
            value: b"two".to_vec(),
            clock: LamportClock {
                counter: 1,
                device_id: "device-b".to_string(),
            },
        };

        let applied = state.apply_entry(tied_entry);
        assert!(applied);
        assert_eq!(state.get("alpha"), Some("two".as_bytes()));
    }

    #[test]
    fn state_updates_counter_on_remote_entry() {
        let mut state = State::new("device-a");
        assert_eq!(state.counter, 0);

        let entry = crate::Entry {
            key: "alpha".to_string(),
            value: b"one".to_vec(),
            clock: LamportClock {
                counter: 42,
                device_id: "device-b".to_string(),
            },
        };

        state.apply_entry(entry);
        assert_eq!(state.counter, 42);
    }

    #[test]
    fn state_apply_entry_skips_older_value() {
        let mut state = State::new("device-a");
        state.set("alpha", b"one".to_vec());

        let older_entry = crate::Entry {
            key: "alpha".to_string(),
            value: b"zero".to_vec(),
            clock: LamportClock {
                counter: 0,
                device_id: "device-b".to_string(),
            },
        };

        let applied = state.apply_entry(older_entry);
        assert!(!applied);
        assert_eq!(state.get("alpha"), Some("one".as_bytes()));
    }

    #[test]
    fn state_snapshot_is_sorted_by_key() {
        let mut state = State::new("device-a");
        state.set("bravo", b"b".to_vec());
        state.set("alpha", b"a".to_vec());

        let snapshot = state.snapshot();
        assert_eq!(snapshot[0].key, "alpha");
        assert_eq!(snapshot[1].key, "bravo");
    }

    #[test]
    fn state_save_and_load_round_trip() {
        let mut state = State::new("device-a");
        state.set("alpha", b"one".to_vec());

        let dir = tempfile::tempdir().expect("tempdir");
        let path: PathBuf = dir.path().join("state.json");
        state.save(&path).expect("save state");

        let loaded = State::load(&path).expect("load state");
        assert_eq!(loaded, state);
    }
}
