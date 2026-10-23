# Portable managed identity for app updates

Fix path-derived Keychain lookup so moving an iOS app data container preserves identity, pairing, encrypted journal and app notes. Keep public function signatures and managed C ABI v1; additive optional migration JSON fields/errors are allowed and documented. No cloud or identity regeneration for existing state.

## Design

Rust stores a small versioned nonsecret identity reference inside the leased private state directory. It names an exact managed secure-store account, independent of future absolute paths. Fresh states use random 64-hex account namespaces. Durable key precedes durable atomic 0600 reference, which precedes first journal. Existing legacy same-path state retains its existing account and writes a reference only after successful authentication/validation/save.

An optional legacy_identity_key candidate is accepted only for an existing regular session.enc with no reference (NotFound specifically). Exact managed-[64 lowercase hex]-identity-v1 syntax is required. Candidate lookup cannot generate/copy keys. It must authenticate the journal and validate certificate/key, identity, manifest, metadata and limits before persisting a reference to the original account. Mismatch candidates never change keys or files. A present reference is authoritative: corrupt/unreadable/unsupported reference, missing key and corrupt journal fail closed with no enumeration or regeneration.

Swift performs normal open first. A dedicated migration-required error permits bounded attributes-only enumeration of strict managed account names in the exact concrete LibreSyncKeychainStore service. Deduplicate/sort and abort overflow; custom stores are not enumerated. Retry only typed candidate mismatch; abort storage unavailable, cancellation, Busy, IO or uncertain persistence. Preserve all old accounts and avoid secret aliases. Expose additive errors in Swift/Kotlin/schema and migration field in managed JSON/core as needed. Do not log key/record data.

## Verification

Test first and observe relocation failure. Cover fresh relocation with records and peers; same-path and moved legacy migration; wrong candidates and malformed state without writes; authoritative missing/corrupt references and unavailable keys; independent profiles sharing one store; enumeration bounds; locator interruption/retry ordering. Run workspace tests, all-features tests, strict clippy, Swift tests and iOS build, Android build/JNI and final package metadata/security checks. Verify real simulator production build updates preserve name, note count and trusted peer identities without temporary recovery helpers. Obtain specification then separate quality/security review before commit and push.

Document state directory copies as migrations/backups preserving identity, not independent device creation, and old-binary downgrade limitations. Update SECURITY, acceptance, SDK guidance and changelog. Keep 0.8.1 unpublished; user requested commit/push, not tag/release.
