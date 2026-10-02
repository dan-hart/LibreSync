# Releasing

Keep all four Rust crates and dependencies, both Cargo locks, nested Tauri manifest/config, Android core/Compose/sample versions and root Swift release manifest aligned. Preserve historical release entries. Use the actual local release date and an immutable `vX.Y.Z` tag. crates.io publication is outside this process.

## Build and verify before tagging

Run ordinary tests in parallel. Coverage alone serializes tests because instrumented short-deadline socket fixtures may exceed their test-client deadline; retain the same source and 75% region threshold without exclusions.

```sh
cargo build --workspace --locked
cargo test --workspace --locked
cargo test -p libresync --all-features --locked
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test -p libresync --release --all-features --locked 'session::network::tests::' -- --ignored --test-threads=1
cargo llvm-cov --workspace --all-features --locked --summary-only --fail-under-regions 75 -- --test-threads=1
scripts/utilities/security-audit.sh
cargo audit --file Cargo.lock
cargo audit --file alwaysOn/libresync-always-on/src-tauri/Cargo.lock
cargo build --locked --manifest-path alwaysOn/libresync-always-on/src-tauri/Cargo.toml
cargo test --locked --manifest-path alwaysOn/libresync-always-on/src-tauri/Cargo.toml
cargo clippy --locked --manifest-path alwaysOn/libresync-always-on/src-tauri/Cargo.toml --all-targets -- -D warnings
node --test alwaysOn/libresync-always-on/tests/*.test.cjs
```

Install matched Rust cross-target standard libraries/compiler, Apple tools, SDK37.0/build-tools37.0.0, NDK27.2.12479018 and JDK17. Linux Tauri needs WebKitGTK4.1, GTK3, Ayatana and librsvg development packages. Review every dependency warning; never ignore advisories to obtain a green report. SECURITY.md qualifies the current Tauri warnings.

## Native assets and root Swift package

```sh
scripts/package-native-sdk.sh X.Y.Z /tmp/libresync-sdk-final
cp /tmp/libresync-sdk-final/Package.release.swift Package.swift
LIBRESYNC_SDK_ARTIFACT_DIR=/tmp/libresync-sdk-final scripts/utilities/check-release-readiness.sh
LIBRESYNC_USE_LOCAL_XCFRAMEWORK=1 swift test
```

The package script rebuilds all five Apple slices and both Android JNI ABIs, licensed core/Compose AARs and Maven metadata. It replaces generated version-specific ZIPs rather than retaining old entries. The complete Swift ZIP retains a local XCFramework manifest; the repository root defaults to the immutable GitHub release URL with the checksum of the exact ZIP bytes. Explicit `LIBRESYNC_USE_LOCAL_XCFRAMEWORK=1` enables source builds/CI before publication. Commit that root manifest **before** tagging; do not upload preview assets early or retag to work around unavailable assets.

Verify Swift tests and generic iOS compilation, Android sample/Compose compilation and all six JNI tests on an actual API37 16 KiB emulator. Record `ro.build.version.sdk`, `ro.product.cpu.abi` and `getconf PAGE_SIZE`; CI requires x86_64 and 16384, with no 4 KiB fallback. Its explicit temporary AVD target correction addresses the upstream `37.0` parsing bug. Check ELF LOAD alignment for both ABIs. Extract final Swift and Maven ZIPs into fresh directories and compile real sample consumers from those bytes. Record source inventory, compiler versions, command logs and SHA256 sums. Native binaries precede the manifest-only checksum edit; record their precise source provenance.

## Publish and verify

Obtain specification and separate quality/security approval on frozen source. Commit/push the branch, open a PR and wait for exact-head CI and approval. Merge, verify main, then tag/push the same immutable source and create its GitHub release with the exact final SDK assets and SHA256SUMS. Verify a fresh remote Git Swift-package consumer only after asset publication. Update the Homebrew tap to the tag archive URL and verified archive checksum, then verify remote tag/release/tap.

Do not promise notarization or Developer ID signing without a real identity and successful verification. Record physical iOS privacy/camera, physical Android, human usability and actual Finder/Dock reopen limits accurately. `check-release-readiness.sh` checks metadata and optionally final asset checksum; it does not certify behavioral acceptance.
