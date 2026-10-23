# Portable iOS identity implementation plan

1. Add failing identity relocation/migration tests; implement durable portable reference and guarded legacy candidate bridge in Rust/managed FFI with additive schema and typed errors.
2. Add failing Swift migration tests; implement bounded exact-service Keychain account enumeration and safe typed retry, preserving cancellation and custom-store behavior. Align Kotlin error decoding.
3. Review specification compliance and code/security quality; fix all important findings. Update security, SDK, changelog and acceptance docs.
4. Build native assets and validate actual simulator reinstall using the production fix (no recovery helper). Run Rust/Swift/Android gates, regenerate exact Swift checksum manifest and package hashes.
5. Inspect full authorized 0.8.1 change set, commit on task/native-sync-polish, push and verify remote head. Leave apps open and release unpublished.
