#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-12.0}"
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-15.0}"
profile=release
flags=(--release)
if [[ "${1:-}" == --debug ]]; then profile=debug;flags=(--profile dev);fi
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target}"
# Cross-target std libraries must match the selected compiler.
if [[ -z "${RUSTC:-}" ]]; then export RUSTC="$(rustup which rustc)";fi
for target in aarch64-apple-darwin x86_64-apple-darwin aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios; do
 cargo build -p libresync-ffi --target "$target" "${flags[@]}"
done
out="$CARGO_TARGET_DIR/apple-sdk/$profile"
mkdir -p "$out/macos" "$out/simulator"
lipo -create "$CARGO_TARGET_DIR/aarch64-apple-darwin/$profile/liblibresync_ffi.a" "$CARGO_TARGET_DIR/x86_64-apple-darwin/$profile/liblibresync_ffi.a" -output "$out/macos/liblibresync_ffi.a"
lipo -create "$CARGO_TARGET_DIR/aarch64-apple-ios-sim/$profile/liblibresync_ffi.a" "$CARGO_TARGET_DIR/x86_64-apple-ios/$profile/liblibresync_ffi.a" -output "$out/simulator/liblibresync_ffi.a"
framework="$root/bindings/swift/LibreSyncFFI.xcframework"
rm -rf "$framework"
xcodebuild -create-xcframework -library "$out/macos/liblibresync_ffi.a" -library "$CARGO_TARGET_DIR/aarch64-apple-ios/$profile/liblibresync_ffi.a" -library "$out/simulator/liblibresync_ffi.a" -output "$framework"
cp bindings/include/libresync.h bindings/swift/Sources/CLibreSync/include/libresync.h
