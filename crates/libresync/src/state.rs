use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{decrypt_blob, encrypt_blob, AppKey, Entry, LamportClock, Result};

/// Local key/value sync state.
///
/// Every entry carries a Lamport clock used for last-writer-wins ordering. In
/// addition the state tracks a local, monotonically increasing *apply
/// sequence* per key. The sequence is never sent on the wire as-is; it is only
/// used to compute deltas ("everything applied locally after sequence N") and
/// to remember, per peer, how far that peer's own sequence has been received.
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub struct State {
    pub device_id: String,
    pub counter: u64,
    pub entries: BTreeMap<String, Entry>,
    /// Local apply sequence. Incremented every time an entry is written.
    #[serde(default)]
    pub sequence: u64,
    /// Apply sequence of the current version of each key.
    #[serde(default)]
    pub sequences: BTreeMap<String, u64>,
    /// Device the current version of a key was received from. Absent for
    /// locally produced versions.
    #[serde(default)]
    pub origins: BTreeMap<String, String>,
    /// For each peer device id: how far that peer's entries have been
    /// received. Used to request deltas.
    #[serde(default)]
    pub peer_cursors: BTreeMap<String, PeerCursor>,
    /// Random identifier of this state's history. It changes when the state
    /// is recreated, so peers holding a cursor into the old history fall back
    /// to a full snapshot instead of missing entries.
    #[serde(default)]
    pub epoch: String,
    #[serde(skip)]
    inbound_origin: Option<String>,
}

/// Position in a peer's apply sequence.
#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
pub struct PeerCursor {
    pub epoch: String,
    pub clock: u64,
}

fn random_epoch() -> String {
    use rand_core::{OsRng, RngCore};
    let mut bytes = [0u8; 8];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl State {
    pub fn new(device_id: impl Into<String>) -> Self {
        Self {
            device_id: device_id.into(),
            counter: 0,
            entries: BTreeMap::new(),
            // Starts at 1 so a cursor clock of 0 always means "never synced".
            sequence: 1,
            sequences: BTreeMap::new(),
            origins: BTreeMap::new(),
            peer_cursors: BTreeMap::new(),
            epoch: random_epoch(),
            inbound_origin: None,
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
        self.entries.insert(key.clone(), entry.clone());
        self.record_write(&key, None);
        entry
    }

    pub fn get(&self, key: &str) -> Option<&[u8]> {
        self.entries.get(key).map(|entry| entry.value.as_slice())
    }

    pub fn remove(&mut self, key: &str) -> Option<Entry> {
        self.sequences.remove(key);
        self.origins.remove(key);
        self.entries.remove(key)
    }

    /// Applies an entry using last-writer-wins on the Lamport clock.
    pub fn apply_entry(&mut self, entry: Entry) -> bool {
        if entry.clock.counter > self.counter {
            self.counter = entry.clock.counter;
        }
        match self.entries.get(&entry.key) {
            Some(existing) if existing.clock >= entry.clock => false,
            _ => {
                let key = entry.key.clone();
                self.entries.insert(key.clone(), entry);
                let origin = self.inbound_origin.clone();
                self.record_write(&key, origin);
                true
            }
        }
    }

    /// Writes an entry unconditionally when it differs from the stored version,
    /// regardless of clock ordering. Used by field-level merges whose result may
    /// carry the *existing* clock but new content.
    pub fn upsert_entry(&mut self, entry: Entry) -> bool {
        if entry.clock.counter > self.counter {
            self.counter = entry.clock.counter;
        }
        if self.entries.get(&entry.key) == Some(&entry) {
            return false;
        }
        let key = entry.key.clone();
        self.entries.insert(key.clone(), entry);
        let origin = self.inbound_origin.clone();
        self.record_write(&key, origin);
        true
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

    /// Current local apply sequence.
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Runs `f` with every write attributed to `origin` (a peer device id), so
    /// the resulting versions are excluded from deltas sent back to that peer.
    pub fn with_inbound_origin<R>(&mut self, origin: &str, f: impl FnOnce(&mut Self) -> R) -> R {
        let previous = self.inbound_origin.replace(origin.to_string());
        let result = f(self);
        self.inbound_origin = previous;
        result
    }

    /// Entries whose current version was applied after local sequence `since`.
    /// Versions received from `exclude_origin` are skipped because that peer
    /// already holds them. A `since` of zero, or one ahead of the local
    /// sequence, yields the full snapshot.
    pub fn delta_since(&self, since: u64, exclude_origin: Option<&str>) -> Vec<Entry> {
        if self.is_full_snapshot_request(since) {
            return self.snapshot();
        }
        self.entries
            .iter()
            .filter(|(key, _)| self.sequences.get(*key).copied().unwrap_or(u64::MAX) > since)
            .filter(|(key, _)| match exclude_origin {
                Some(origin) => self.origins.get(*key).map(String::as_str) != Some(origin),
                None => true,
            })
            .map(|(_, entry)| entry.clone())
            .collect()
    }

    /// True when `since` cannot be served as a delta and a full snapshot is
    /// required instead.
    pub fn is_full_snapshot_request(&self, since: u64) -> bool {
        since == 0 || since > self.sequence
    }

    /// Cursor into `device_id`'s history, or an empty cursor (full snapshot)
    /// when the peer has never been synced.
    pub fn peer_cursor(&self, device_id: &str) -> PeerCursor {
        self.peer_cursors.get(device_id).cloned().unwrap_or_default()
    }

    pub fn set_peer_cursor(&mut self, device_id: &str, cursor: PeerCursor) {
        self.peer_cursors.insert(device_id.to_string(), cursor);
    }

    /// Resolves a cursor a peer holds into *our* history to a sequence usable
    /// with [`State::delta_since`]. A cursor from another epoch is unknown and
    /// yields `0` (full snapshot).
    pub fn resolve_cursor(&self, cursor: &PeerCursor) -> u64 {
        if cursor.epoch == self.epoch {
            cursor.clock
        } else {
            0
        }
    }

    /// Cursor describing the current local position for a peer to store.
    pub fn current_cursor(&self) -> PeerCursor {
        PeerCursor {
            epoch: self.epoch.clone(),
            clock: self.sequence,
        }
    }

    /// Forgets delta cursors so the next sync with every peer exchanges a full
    /// snapshot.
    pub fn reset_peer_cursors(&mut self) {
        self.peer_cursors.clear();
    }

    fn record_write(&mut self, key: &str, origin: Option<String>) {
        self.sequence = self.sequence.saturating_add(1);
        self.sequences.insert(key.to_string(), self.sequence);
        match origin {
            Some(origin) => {
                self.origins.insert(key.to_string(), origin);
            }
            None => {
                self.origins.remove(key);
            }
        }
    }

    /// Assigns sequences to entries loaded from state files written before
    /// delta tracking existed, so they are included in the next delta.
    fn normalize(mut self) -> Self {
        if self.epoch.is_empty() {
            self.epoch = random_epoch();
        }
        if self.sequence == 0 {
            self.sequence = 1;
        }
        let missing: Vec<String> = self
            .entries
            .keys()
            .filter(|key| !self.sequences.contains_key(*key))
            .cloned()
            .collect();
        for key in missing {
            self.sequence = self.sequence.saturating_add(1);
            self.sequences.insert(key, self.sequence);
        }
        self.sequences.retain(|key, _| self.entries.contains_key(key));
        self.origins.retain(|key, _| self.entries.contains_key(key));
        self
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let serialized = serde_json::to_vec_pretty(self)?;
        fs::write(path, serialized)?;
        Ok(())
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let data = fs::read(path)?;
        let state: State = serde_json::from_slice(&data)?;
        Ok(state.normalize())
    }

    pub fn save_encrypted(&self, app_key: &AppKey, path: impl AsRef<Path>) -> Result<()> {
        let serialized = serde_json::to_vec(self)?;
        let encrypted = encrypt_blob(app_key, &serialized, b"libresync-state")?;
        fs::write(path, encrypted)?;
        Ok(())
    }

    pub fn load_encrypted(app_key: &AppKey, path: impl AsRef<Path>) -> Result<Self> {
        let data = fs::read(path)?;
        let decrypted = decrypt_blob(app_key, &data, b"libresync-state")?;
        let state: State = serde_json::from_slice(&decrypted)?;
        Ok(state.normalize())
    }

    pub fn load_maybe_encrypted(app_key: &AppKey, path: impl AsRef<Path>) -> Result<Self> {
        let data = fs::read(&path)?;
        let state: State = match decrypt_blob(app_key, &data, b"libresync-state") {
            Ok(decrypted) => serde_json::from_slice(&decrypted)?,
            Err(_) => serde_json::from_slice(&data)?,
        };
        Ok(state.normalize())
    }
}

#[cfg(test)]
mod tests {
    use super::{PeerCursor, State};
    use crate::{AppKey, LamportClock};
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

    #[test]
    fn state_save_and_load_encrypted_round_trip() {
        let mut state = State::new("device-a");
        state.set("alpha", b"one".to_vec());
        let app_key = AppKey::generate().expect("app key");

        let dir = tempfile::tempdir().expect("tempdir");
        let path: PathBuf = dir.path().join("state.enc");
        state
            .save_encrypted(&app_key, &path)
            .expect("save encrypted state");

        let loaded = State::load_encrypted(&app_key, &path).expect("load encrypted state");
        assert_eq!(loaded, state);
    }

    #[test]
    fn state_load_maybe_encrypted_accepts_plaintext() {
        let mut state = State::new("device-a");
        state.set("alpha", b"one".to_vec());
        let app_key = AppKey::generate().expect("app key");

        let dir = tempfile::tempdir().expect("tempdir");
        let path: PathBuf = dir.path().join("state.json");
        state.save(&path).expect("save plaintext state");

        let loaded = State::load_maybe_encrypted(&app_key, &path).expect("load maybe");
        assert_eq!(loaded, state);
    }

    #[test]
    fn state_load_maybe_encrypted_accepts_ciphertext() {
        let mut state = State::new("device-a");
        state.set("alpha", b"one".to_vec());
        let app_key = AppKey::generate().expect("app key");

        let dir = tempfile::tempdir().expect("tempdir");
        let path: PathBuf = dir.path().join("state.enc");
        state
            .save_encrypted(&app_key, &path)
            .expect("save encrypted state");

        let loaded = State::load_maybe_encrypted(&app_key, &path).expect("load maybe");
        assert_eq!(loaded, state);
    }

    #[test]
    fn delta_since_zero_returns_full_snapshot() {
        let mut state = State::new("device-a");
        state.set("alpha", b"one".to_vec());
        state.set("beta", b"two".to_vec());
        assert_eq!(state.delta_since(0, None).len(), 2);
        assert!(state.is_full_snapshot_request(0));
        assert!(state.is_full_snapshot_request(state.sequence() + 1));
    }

    #[test]
    fn delta_since_returns_only_newer_entries() {
        let mut state = State::new("device-a");
        state.set("alpha", b"one".to_vec());
        let cursor = state.sequence();
        state.set("beta", b"two".to_vec());
        let delta = state.delta_since(cursor, None);
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].key, "beta");
        assert!(state.delta_since(state.sequence(), None).is_empty());
    }

    #[test]
    fn delta_excludes_entries_received_from_peer() {
        let mut state = State::new("device-a");
        state.set("alpha", b"one".to_vec());
        let cursor = state.sequence();
        let remote = crate::Entry {
            key: "beta".to_string(),
            value: b"two".to_vec(),
            clock: LamportClock {
                counter: 7,
                device_id: "device-b".to_string(),
            },
        };
        state.with_inbound_origin("device-b", |state| state.apply_entry(remote));
        assert!(state.delta_since(cursor, Some("device-b")).is_empty());
        assert_eq!(state.delta_since(cursor, Some("device-c")).len(), 1);

        // A local overwrite clears the origin again.
        state.set("beta", b"three".to_vec());
        assert_eq!(state.delta_since(cursor, Some("device-b")).len(), 1);
    }

    #[test]
    fn upsert_entry_writes_equal_clock_with_new_content() {
        let mut state = State::new("device-a");
        let first = state.set("alpha", b"one".to_vec());
        let mut merged = first.clone();
        merged.value = b"merged".to_vec();
        assert!(state.upsert_entry(merged.clone()));
        assert_eq!(state.get("alpha"), Some("merged".as_bytes()));
        assert!(!state.upsert_entry(merged));
    }

    #[test]
    fn peer_cursors_round_trip() {
        let mut state = State::new("device-a");
        assert_eq!(state.peer_cursor("device-b"), PeerCursor::default());
        let cursor = PeerCursor {
            epoch: "abc".to_string(),
            clock: 9,
        };
        state.set_peer_cursor("device-b", cursor.clone());
        assert_eq!(state.peer_cursor("device-b"), cursor);
        state.reset_peer_cursors();
        assert_eq!(state.peer_cursor("device-b").clock, 0);
    }

    #[test]
    fn cursors_from_another_epoch_resolve_to_full_snapshot() {
        let mut state = State::new("device-a");
        state.set("alpha", b"one".to_vec());
        let current = state.current_cursor();
        assert_eq!(state.resolve_cursor(&current), state.sequence());
        let foreign = PeerCursor {
            epoch: "other".to_string(),
            clock: state.sequence(),
        };
        assert_eq!(state.resolve_cursor(&foreign), 0);
        assert!(!state.epoch.is_empty());
        assert_ne!(State::new("x").epoch, State::new("y").epoch);
    }

    #[test]
    fn legacy_state_files_gain_sequences_on_load() {
        let legacy = r#"{"device_id":"device-a","counter":1,"entries":{"alpha":{"key":"alpha","value":[1],"clock":{"counter":1,"device_id":"device-a"}}}}"#;
        let dir = tempfile::tempdir().expect("tempdir");
        let path: PathBuf = dir.path().join("legacy.json");
        std::fs::write(&path, legacy).expect("write");
        let loaded = State::load(&path).expect("load");
        assert!(!loaded.epoch.is_empty());
        assert_eq!(loaded.sequences.get("alpha"), Some(&2));
        assert!(!loaded.is_full_snapshot_request(1));
        assert_eq!(loaded.delta_since(0, None).len(), 1);
    }

    #[test]
    fn remove_clears_tracking() {
        let mut state = State::new("device-a");
        state.set("alpha", b"one".to_vec());
        state.remove("alpha");
        assert!(state.sequences.is_empty());
        assert!(state.delta_since(0, None).is_empty());
    }
}
