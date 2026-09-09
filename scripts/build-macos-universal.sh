#!/usr/bin/env bash
# Builds libresync-ffi as a universal (arm64 + x86_64) static library and wraps
# it in an XCFramework consumed by bindings/swift/Package.swift.
#
# Usage: scripts/build-macos-universal.sh [--debug]
# Requires: macOS, Xcode command line tools, and both Rust targets:
#   rustup target add aarch64-apple-darwin x86_64-apple-darwin
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: this script builds Apple universal binaries and must run on macOS" >&2
  exit 1
fi

profile=release
cargo_flags=(--release)
if [[ "${1:-}" == "--debug" ]]; then
  profile=debug
  cargo_flags=()
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

targets=(aarch64-apple-darwin x86_64-apple-darwin)
for target in "${targets[@]}"; do
  echo "==> cargo build -p libresync-ffi --target $target ($profile)"
  cargo build -p libresync-ffi --target "$target" "${cargo_flags[@]}"
done

out="target/universal-apple-darwin/$profile"
mkdir -p "$out"
lipo -create \
  "target/aarch64-apple-darwin/$profile/liblibresync_ffi.a" \
  "target/x86_64-apple-darwin/$profile/liblibresync_ffi.a" \
  -output "$out/liblibresync_ffi.a"
lipo -info "$out/liblibresync_ffi.a"

framework="bindings/swift/LibreSyncFFI.xcframework"
rm -rf "$framework"
xcodebuild -create-xcframework \
  -library "$out/liblibresync_ffi.a" \
  -output "$framework"

# Keep the single C header the Swift package uses in sync.
cp bindings/include/libresync.h bindings/swift/Sources/CLibreSync/include/libresync.h

echo "==> wrote $framework"
echo "Build and test the Swift package with:"
echo "    (cd bindings/swift && swift build && swift test)"
