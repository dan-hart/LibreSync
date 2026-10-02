# Managed companion and app contracts

`CompanionManager` owns one `Session` per independently trusted app space.
Persisted space IDs are random opaque identifiers, not app IDs. State directories,
KeyStore prefixes, device identities, transport groups, local storage keys, trust,
receipts and backup keys are isolated even for two spaces belonging to one app.
The registry uses owner-only atomic writes with file and parent-directory fsync;
a manager lease rejects competing instances. Uncertain registry durability fences
all runtime sessions until reopen. Pause/resume/remove persist registry intent
before changing runtime state; failed pre-publication writes leave runtime intact.

## Declare supported applications

An invitation does not register an adapter policy. The built-in catalog contains
`momentum_manifest()` with `MomentumAdapter` and `notes_manifest()` with a declared
`RecordsAdapter`. Matching app ID alone is insufficient. Schema version, adapter
IDs, namespace, schema and transactional capability must match exactly.

Applications/operators can provide `open_with_catalog(..., Vec<AppSupport>)`.
Each `AppSupport` pairs a trusted exact manifest with its pure transactional
adapter implementations. Descriptor mismatch is rejected. Reopening a persisted
space requires that exact policy again; an absent adapter produces a compatibility
error rather than a generic JSON-file fallback. Adapter hooks validate/stage
logical records only and must not mutate external app databases or files.

`notes_manifest()` is public for Rust/native SDK samples:

| Field | Value |
| --- | --- |
| App ID | `io.libresync.Notes` |
| Friendly app | `Local notes sample` |
| Schema version | `1` |
| Adapter ID | `records` |
| Namespace | `io.libresync.Notes` |
| Schema | `logical-records-v1` |
| Transactional | `true` |

Its merge is whole-record last-writer-wins with retained tombstones. It is a
sample app contract, not permission to connect arbitrary JSON files or other
notes products. `companion_app` demonstrates a real advertised peer and invitation.
The low-level legacy `Engine` and CLI remain explicit independent APIs.

## Store and forward

Managed incoming batches atomically capture records and authenticated receipts
before acknowledging **Stored**. `Session::import_records` imports app-produced
logical records without rewriting clocks, tombstones or values; schema and
aggregate/record size limits still apply atomically. It does not grant trust,
approve bootstrap, or issue application **Applied** receipts.

The app captures `application_inbox()`, performs its own durable transaction
with operation deduplication, then acknowledges that exact captured inbox.
Preserve its checkpoint proofs verbatim. Never acknowledge current receipts for
an older app snapshot. Companion restart retains trust, records and receipt
state; a disconnected producer is unnecessary for forwarding to another app peer.

Live operations and tombstones are retained. No age-based compaction runs in the
companion. A future compaction policy needs app-defined snapshot coverage and
receipts from every enrolled peer; it must not be inferred from a newer timestamp.
Core export bounds remain 64 MiB per record and 128 MiB per authoritative batch.

## Recovery and secure storage

The tray uses native macOS Keychain (binary API, no secrets in process arguments)
or Linux Secret Service through `secret-tool`. Native item-not-found and the
unambiguous CLI no-match outcomes alone mean absence. Locked/denied/unavailable,
empty/corrupt output and missing backup keys beside existing backups fail closed.
Secure-backend errors do not trigger file fallback. The legacy `SecurityCliKeyStore`
remains available to existing consumers; its setter still passes values to the
`security` process, so the managed tray uses the native backend instead.

Each space keeps five encrypted logical backups and five encrypted pre-merge
copies under an explicit retention policy. Pruning copies is unrelated to live
record/tombstone compaction. Backups use a separate stable key. Export is private
unencrypted app data and has no state-restoration or receipt effects. Immutable
operation logs require app-aware recovery; generic whole-state rollback is absent.
Removed spaces retain their encrypted storage and keys as archives. No remote
wipe or group-wide key erasure is promised.

## Legacy migration

Existing config/data/state/key/trust paths are never overwritten or enrolled
automatically. The tray uses `com.codedbydan.libresync.alwayson` for new managed
data. It identifies the previous LibreSync configuration within the historical
Tauri default `com.tauri.dev` application-data folder by the actual config fields;
it does not assume other files in that shared parent belong to LibreSync.

Make an explicit recovery copy, review the app’s managed contract, and create a
new supported app space. An explicit legacy archive contains owner-only copies of
the config and the existing data/state files named by it. Recovery copies can
contain old plaintext key material, so the archive is private and must be protected.
Legacy trust, automatic approval, shared secrets and generic JSON adapters are
not activated. Archive operations report failures; a partial archive is never
reported as a completed migration. Original files remain available for recovery.

The headless daemon has an explicit `archive-legacy CONFIG` maintenance command.
The current Momentum migration is separately reviewable in
[MOMENTUM-MANAGED-MIGRATION.md](MOMENTUM-MANAGED-MIGRATION.md).

Explicit legacy recovery archives retain the latest five complete copies. Failed copies are removed before publication. Retention never prunes original config/data/state paths or managed live records.

## Isolated native UI inspection

Debug builds accept explicit `LIBRESYNC_DEVELOPMENT_ROOT` (an absolute isolated
path), `LIBRESYNC_DEVELOPMENT_FILE_KEYS=1`, and
`LIBRESYNC_DEVELOPMENT_DEVICE_NAME` environment options, including through a
macOS debug bundle's `LSEnvironment`. File keys require the explicit root, and the
dashboard displays development mode. Production builds reject these environment
options; ordinary launches use the OS secure store. The equivalent command-line
options are `--development-root`, `--development-file-keys`, and `--device-name`.

Removed devices remain visible as removed with local data preserved. Reconnect
requires an authenticated fresh invitation from that same device and explicit
repair; Resume cannot restore revoked trust. Pasted older protocol invitations
give upgrade guidance before strict decoding, without trusting their metadata.

The resolved Tauri Linux GTK3 dependencies retain the glib `VariantStrIter`
unsoundness warning and compile-time `proc-macro-error` maintenance warning. See
[the exact advisory and reachability qualification](../SECURITY.md#tauri-dependency-audit-qualification).
