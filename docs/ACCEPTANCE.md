# Managed release acceptance map

These executable scenarios map the ten UX/DX improvements to implementation evidence. The map describes assertions; the release operator records final command results and exact source/artifact hashes separately. Passing tests do not establish two-minute human usability or physical-device privacy/camera acceptance.

| Improvement | Executable acceptance | Integration guide |
| --- | --- | --- |
| 1. Managed lifecycle | `managed_session::two_sessions_pair_and_automatically_sync_edits_resume_and_reopen`, duplicate-open and failed-persistence cases; `managed_two_peers` example | [Session ownership](MANAGED-SESSION.md) |
| 2. QR/code pairing | `pairing::socket_tests` covers invalid transcripts, no key before confirmation/prepared phase, expired/revoked invitation, replay and concurrent claims; managed `session_code_entry_uses_advertised_descriptor_and_rechecks_contract`; native QR roundtrip | [Pairing boundary](../SECURITY.md#managed-pairing-boundary) |
| 3. Named compatible discovery | managed independent-process discovery; companion `nearby_hints_refuse_other_apps_schema_digest_old_protocol_and_missing_metadata`; Swift real system Bonjour registration/resolve/refresh/withdrawal | [Native discovery](../bindings/swift/README.md) |
| 4. Permission/lifecycle guidance | managed `platform_unknown_is_preserved_and_slow_event_consumers_do_not_block_local_commits`; Android `secureStoreDistinguishesAbsentDeniedAndCorruptFilesAndActualApi37Permission` and rapid lifecycle transitions | [Apple setup](../bindings/swift/README.md), [Android setup](../bindings/kotlin/README.md) |
| 5. Automatic resume/reconnect | managed lifecycle/reopen, `wake_syncs_promptly_even_with_long_periodic_catchup`, `local_edits_while_paused_remain_pending_and_resume_automatically`; native reconnect | [Scheduling](MANAGED-SESSION.md) |
| 6. Truthful status | managed `remote_stored_is_distinct_from_app_applied_and_removal_preserves_data`; core captured-export concurrent edit and checkpoint proof replay tests; Swift/Android deliberate save failure then exact durable acknowledgement | [App transaction](MANAGED-SESSION.md#truthful-status-and-app-processing) |
| 7. Safe first sync | managed populated bootstrap/stale consent, cancel/restart, encrypted premerge recovery, atomic multi-adapter validation; optimized 32/64 MiB transfer and populated-pair tests | [Bootstrap](MANAGED-SESSION.md#records-and-atomic-merge) |
| 8. Multi-app companion | `companion::actual_momentum_stored_restart_forward_then_app_applied_and_group_isolation`, independent apps/keys restart, exact Momentum wire collision/tombstone, legacy migration, backup fault tests; Tauri Rust and dashboard Node tests | [Companion](COMPANION.md), [pinned Momentum opt-in patch](MOMENTUM-MANAGED-MIGRATION.md) |
| 9. Repair/pause/remove | managed removal socket cancellation/fencing, certificate/repair and preserved-data cases; companion encrypted backup and pause/remove persistence | [Trust repair](MANAGED-SESSION.md#connect-devices) |
| 10. Native SDKs and components | FFI24 ownership/ABI and monotonic-ID boundary tests, Swift12 runtime tests, Android6 real JNI/Keystore/Flow tests; five Apple slices, generic iOS compile, both Android ABIs/16 KiB LOAD alignment, licensed core/Compose AARs/sample and freshly extracted release consumers | [SDK installation](SDK.md), [managed ABI](MANAGED-C-ABI.md) |

Run the ordinary parallel workspace and all-feature core suites, plus the separate optimized giant tests, with commands in [the release checklist](RELEASING.md). Swift tests exercise actual macOS native sessions. Local Android runtime evidence uses ARM64 API37/16 KiB; CI separately requires actual x86_64 API37/16 KiB SDK/ABI/page-size assertions before its runtime suite. A failed or unavailable CI run is not runtime proof.

## Compatibility and proof limits

Legacy Engine/C/Swift APIs remain. Historical raw Kotlin Engine/backup functions explicitly refuse because the old wrapper never implemented JNI. Managed pairing version2 does not downgrade. Native generic policy is reviewed transactional `logical-records-v1`; arbitrary app schemas need a pure Rust policy. Session storage and app database storage are distinct; Stored means durable peer storage, Applied requires the app's coherent durable records-and-receipts save.

Momentum's opt-in patch is pinned to `766064bfa340a391d6a9465b60a030e1dc0c9f95`; existing app runtime/UI is not automatically migrated. LAN operation is local-only with no cloud service requirement. iOS suspension and Android background limits remain. Current listener defaults to IPv4; no IPv6 runtime support claim. Physical iOS privacy/camera, physical Android hardware, two-minute human usability, actual Finder/Dock reopen and notarized/signed GUI acceptance require additional evidence and are unverified here.

## Native Nearby Sync demos

The v0.8.0 Swift SDK includes the reusable `LibreSyncNearbySyncView` and persistent generated device naming. The [macOS](../demos/NearbySyncMac/README.md) and [iOS](../demos/NearbySyncIOS/README.md) Nearby Notes demos are repository examples available at the exact `v0.8.0` tag; they are not bundled in the Swift SDK package. From that checkout, `scripts/run-nearby-sync-demo.sh --two` builds the local Rust library and opens two isolated macOS application profiles. The historical v0.7.0 assets remain unchanged.

`NearbySyncModelTests` covers compatibility without an invitation, live code pairing, invalid/wrong codes, expiry, duplicate offers, blocked-handshake cancellation, invitation cleanup, queued lifecycle stop, retry recovery, permission evidence and inviter success. `DemoIntegrationTests.testTwoDemoHostsDiscoverPairApplyRenameAndReopen` uses real Bonjour and authenticated code pairing, explicit merge consent, production note persistence before Applied acknowledgement, authenticated rename propagation, and durable restart. Temporary state and ephemeral test secure stores isolate those tests; they do not prove platform Keychain privacy behavior. Separate demo storage tests cover failed persistence blocking acknowledgement and coherent records/receipt readback.

The native app was built and its screen inspected on macOS. GUI pairing automation stopped when the computer-use connection closed; that click-through is unverified. Physical-device/iOS privacy and cross-Mac networking remain separate acceptance checks. An existing short-expiry Bonjour visibility assertion was intermittent once and passed unchanged on isolation/full-suite reruns; its root cause is not established.

## iOS Nearby Notes demo

The [iOS demo](../demos/NearbySyncIOS/README.md) compiles the Mac demo’s model and storage sources into a native iPhone/iPad shell. Simulator and development-signed physical-device builds are separate gates. The deployment helper verifies the explicit physical device, app signature, installed bundle and running process; a locked phone can permit installation while refusing launch.

Shared-host regression tests exercise backgrounding during startup and rename, same-session resume, note/identity/trust preservation, and actual two-host discovery/pairing/Applied receipts. iOS storage excludes the state root from device backups and applies complete file protection to plaintext app inbox files before acknowledgement. Those host tests do not establish physical Local Network permission behavior, camera acceptance or saved-note delivery between an iPhone and Mac; use the guide’s manual acceptance flow.

## Native view polish validation (0.8.1 source)

The platform-native polish was compiled for macOS, iOS Simulator and Android API 37. Swift SDK tests (39), Mac demo tests (13), and Android JNI tests (6, ARM64, 16 KiB pages) passed. Native previews covered the macOS grouped screen, iOS grouped screen and pairing sheet in light/dark appearance, maximum iOS Dynamic Type, and the Android sample at 320dp width. Source reviews checked expiry, consent, scrolling ownership and persistent dialog actions. Physical devices and complete Android large-font/keyboard pairing acceptance were not exercised in this pass.

## App-update identity preservation (0.8.1 source)

The path-derived secure-store lookup failure was reproduced before the fix in Rust and Swift native tests. Managed state now carries a private, nonsecret `identity-ref.json`; the production Swift Keychain adapter migrates markerless legacy state only after Rust authenticates its journal. Source/specification and independent security reviews covered failed candidates, malformed/oversized/version-invalid/symlink references, locked/missing keys, independent profiles, prepared recovery data, cancellation and commit interruptions.

A disposable iOS 27.2 simulator was seeded using the v0.8.0 SDK with one trusted peer and one saved note. A test app using the production 0.8.1 SDK, with no recovery/alias helper, was installed twice. Both installations changed the iOS data-container path. After the legacy migration and again after the subsequent update, digest-only comparison confirmed identical device identity, friendly name, trusted peer identity/fingerprint/revocation fields, note records and count. The reference was mode 0600. Test logging exported only counts and digests, not record values or credentials.

The original Snug Peach simulator was then updated to the official production demo without the temporary recovery helper and continued to display all three saved test notes. No identity was regenerated, trust reset, journal deleted or peer re-paired. Simulator checks do not establish physical iOS privacy/camera acceptance, app uninstall recovery, restoration without secure-store credentials or unsupported older-SDK downgrades. No release was published as part of this work.
