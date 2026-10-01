//! Transactional logical adapters whose data is owned by the Session journal.
//! These hooks are pure: they validate and produce serializable records. They
//! must not write an external database or file. Such side effects cannot be
//! included in this coordinator's atomic commit.
use super::*;
use crate::AdapterDescriptor;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdapterInspection {
    pub records: usize,
    pub live_records: usize,
    pub revision: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreparedChange {
    pub id: String,
    pub adapter: String,
    pub expected_revision: u64,
    pub staged: Vec<ManagedRecord>,
    pub recovery: Vec<ManagedRecord>,
}
pub trait ManagedAdapter: Send + Sync {
    fn descriptor(&self) -> AdapterDescriptor;
    fn inspect(&self, records: &[ManagedRecord], revision: u64) -> AdapterInspection {
        AdapterInspection {
            records: records.len(),
            live_records: records.iter().filter(|r| !r.deleted).count(),
            revision,
        }
    }
    fn export(&self, records: &[ManagedRecord]) -> Result<Vec<ManagedRecord>> {
        Ok(records.to_vec())
    }
    fn preview(
        &self,
        local: &[ManagedRecord],
        incoming: &[ManagedRecord],
        revision: u64,
    ) -> Result<PreparedChange> {
        self.prepare(local, incoming, revision)
    }
    fn prepare(
        &self,
        local: &[ManagedRecord],
        incoming: &[ManagedRecord],
        revision: u64,
    ) -> Result<PreparedChange>;
    fn commit(
        &self,
        prepared: &PreparedChange,
        current_revision: u64,
    ) -> Result<Vec<ManagedRecord>> {
        if prepared.adapter != self.descriptor().id
            || prepared.expected_revision != current_revision
        {
            return fail("stale adapter transaction");
        }
        Ok(prepared.staged.clone())
    }
    fn recover(&self, prepared: &PreparedChange) -> Result<Vec<ManagedRecord>> {
        if prepared.adapter != self.descriptor().id {
            return fail("adapter recovery mismatch");
        }
        Ok(prepared.staged.clone())
    }
}
/// Last-writer-wins logical records with durable tombstones. Applications can
/// replace its pure prepare hook to enforce domain validation/merge semantics.
#[derive(Clone, Debug)]
pub struct RecordsAdapter {
    descriptor: AdapterDescriptor,
}
impl RecordsAdapter {
    pub fn new(descriptor: AdapterDescriptor) -> Self {
        Self { descriptor }
    }
}
impl ManagedAdapter for RecordsAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        self.descriptor.clone()
    }
    fn prepare(
        &self,
        local: &[ManagedRecord],
        incoming: &[ManagedRecord],
        revision: u64,
    ) -> Result<PreparedChange> {
        let mut merged: BTreeMap<_, _> = local.iter().map(|r| (r.id.clone(), r.clone())).collect();
        for next in incoming {
            if next.adapter != self.descriptor.id {
                return fail("record adapter mismatch");
            }
            if let Some(old) = merged.get(&next.id) {
                if old.clock == next.clock && old != next {
                    return fail("unequal payload at identical logical clock");
                }
                if old.clock >= next.clock {
                    continue;
                }
            }
            merged.insert(next.id.clone(), next.clone());
        }
        Ok(PreparedChange {
            id: random_id(),
            adapter: self.descriptor.id.clone(),
            expected_revision: revision,
            staged: merged.into_values().collect(),
            recovery: local.to_vec(),
        })
    }
}
impl SessionConfig {
    pub fn with_adapter(mut self, adapter: Arc<dyn ManagedAdapter>) -> Result<Self> {
        let descriptor = adapter.descriptor();
        if !descriptor.transactional || !self.metadata.manifest.adapters.contains(&descriptor) {
            return fail("managed adapter descriptor differs from manifest");
        }
        self.adapters.insert(descriptor.id.clone(), adapter);
        Ok(self)
    }
}
impl Inner {
    pub(super) fn prepare_records(
        &self,
        e: &Envelope,
        incoming: &[ManagedRecord],
    ) -> Result<Vec<PreparedChange>> {
        let mut prepared = Vec::new();
        for (id, adapter) in &self.config.adapters {
            let local: Vec<_> = e
                .records
                .values()
                .filter(|r| &r.adapter == id)
                .cloned()
                .collect();
            let incoming: Vec<_> = incoming
                .iter()
                .filter(|r| &r.adapter == id)
                .cloned()
                .collect();
            if incoming.is_empty() {
                continue;
            }
            let staged = adapter.prepare(&local, &incoming, e.revision)?;
            if staged.adapter != *id
                || staged.expected_revision != e.revision
                || staged.recovery != local
            {
                return fail("invalid adapter preparation");
            }
            validate_records(id, &local, &staged.staged)?;
            prepared.push(staged);
        }
        Ok(prepared)
    }
    pub(super) fn commit_records(
        &self,
        e: &mut Envelope,
        prepared: &[PreparedChange],
        origin: Option<&str>,
    ) -> Result<()> {
        let expected = e.revision;
        // Validate every adapter before publishing any of their changes.
        let results = prepared
            .iter()
            .map(|p| {
                let adapter = self
                    .config
                    .adapters
                    .get(&p.adapter)
                    .ok_or_else(|| Error::Protocol("adapter missing".into()))?;
                let records = adapter.commit(p, expected)?;
                validate_records(&p.adapter, &p.recovery, &records)?;
                Ok(records)
            })
            .collect::<Result<Vec<_>>>()?;
        for records in results {
            for r in records {
                let k = record_key(&r.adapter, &r.id)?;
                if e.records.get(&k) != Some(&r) {
                    e.revision = e
                        .revision
                        .checked_add(1)
                        .ok_or_else(|| Error::Protocol("revision exhausted".into()))?;
                    e.records.insert(k.clone(), r);
                    e.sequences.insert(k.clone(), e.revision);
                    match origin {
                        Some(peer) => {
                            e.origins.insert(k, peer.into());
                        }
                        None => {
                            e.origins.remove(&k);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
fn validate_records(
    adapter: &str,
    before: &[ManagedRecord],
    after: &[ManagedRecord],
) -> Result<()> {
    let mut ids = std::collections::BTreeSet::new();
    for r in after {
        if r.adapter != adapter
            || r.id.is_empty()
            || r.id.len() > 1024
            || r.value.len() > compact::MAX_RECORD_BYTES
            || !ids.insert(&r.id)
        {
            return fail_code(
                SessionErrorCode::InvalidRecord,
                "invalid adapter staged record",
            );
        }
    }
    if before.iter().any(|r| !ids.contains(&r.id)) {
        return fail("managed adapters must preserve tombstones instead of dropping records");
    }
    Ok(())
}

impl Inner {
    pub(super) fn preview_records(&self, e: &Envelope, incoming: &[ManagedRecord]) -> Result<()> {
        for (id, adapter) in &self.config.adapters {
            let local: Vec<_> = e
                .records
                .values()
                .filter(|r| &r.adapter == id)
                .cloned()
                .collect();
            let incoming: Vec<_> = incoming
                .iter()
                .filter(|r| &r.adapter == id)
                .cloned()
                .collect();
            let prepared = adapter.preview(&local, &incoming, e.revision)?;
            if prepared.adapter != *id
                || prepared.expected_revision != e.revision
                || prepared.recovery != local
            {
                return fail("invalid adapter preview");
            }
            validate_records(id, &local, &prepared.staged)?;
        }
        Ok(())
    }
    pub(super) fn export_records(&self, records: &[ManagedRecord]) -> Result<Vec<ManagedRecord>> {
        let mut exported = Vec::new();
        for (id, adapter) in &self.config.adapters {
            let selected: Vec<_> = records
                .iter()
                .filter(|r| &r.adapter == id)
                .cloned()
                .collect();
            let result = adapter.export(&selected)?;
            let mut ids = std::collections::BTreeSet::new();
            if result
                .iter()
                .any(|r| !selected.contains(r) || !ids.insert(&r.id))
            {
                return fail("adapter export must preserve selected logical records");
            }
            exported.extend(result);
        }
        Ok(exported)
    }
}
impl Session {
    pub fn adapter_inspections(&self) -> Result<BTreeMap<String, AdapterInspection>> {
        let e = lock(&self.inner.envelope)?;
        Ok(self
            .inner
            .config
            .adapters
            .iter()
            .map(|(id, a)| {
                let records: Vec<_> = e
                    .records
                    .values()
                    .filter(|r| &r.adapter == id)
                    .cloned()
                    .collect();
                (id.clone(), a.inspect(&records, e.revision))
            })
            .collect())
    }
}
