use super::*;
use std::io::Write;
struct SizeBudget(usize);
impl Write for SizeBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| std::io::Error::other("managed exportable state exceeds 128 MiB"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn bounded_json(value: &impl Serialize) -> Result<()> {
    serde_json::to_writer(SizeBudget(compact::MAX_BATCH_BYTES), value).map_err(|_| Error::Managed {
        code: SessionErrorCode::InvalidRecord,
        message: "managed exportable state exceeds 128 MiB serialized batch budget".into(),
    })
}
pub(super) fn batch(batch: &ExportBatch) -> Result<()> {
    bounded_json(batch)
}
pub(super) fn state(e: &Envelope) -> Result<()> {
    if e.epoch.is_empty() || e.epoch.len() > 128 {
        return fail_code(
            SessionErrorCode::InvalidRecord,
            "invalid managed source epoch",
        );
    }
    if e.records.len() > 100_000 {
        return fail_code(SessionErrorCode::InvalidRecord, "too many managed records");
    }
    for record in e.records.values() {
        if record.value.len() > compact::MAX_RECORD_BYTES
            || record.id.is_empty()
            || record.id.len() > 1024
            || record.clock.device_id.len() > 128
            || !e
                .metadata
                .manifest
                .adapters
                .iter()
                .any(|a| a.id == record.adapter)
        {
            return fail_code(
                SessionErrorCode::InvalidRecord,
                "invalid managed record or size bound",
            );
        }
    }
    let raw_bytes = e
        .records
        .values()
        .try_fold(0usize, |n, r| n.checked_add(r.value.len()))
        .ok_or_else(|| Error::Managed {
            code: SessionErrorCode::InvalidRecord,
            message: "managed record size overflow".into(),
        })?;
    if raw_bytes >= compact::MAX_BATCH_BYTES / 4 * 3 {
        return fail_code(
            SessionErrorCode::InvalidRecord,
            "managed bytes alone exceed exportable batch budget",
        );
    }
    #[derive(Serialize)]
    struct CompleteBatch<'a> {
        checkpoint: ManagedReceipt,
        records: Vec<&'a ManagedRecord>,
        full: bool,
    }
    // Reserve maximum checkpoint field sizes, independent of the current cursor.
    bounded_json(&CompleteBatch {
        checkpoint: ManagedReceipt {
            epoch: "\0".repeat(128),
            sequence: u64::MAX,
            proof: vec![255; 32],
        },
        records: e.records.values().collect(),
        full: false,
    })
}
