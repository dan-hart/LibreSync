#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
build_only=false
two=false
profile=primary
while [[ $# -gt 0 ]]; do
  case "$1" in
    --build-only) build_only=true; shift ;;
    --two) two=true; shift ;;
    --profile) [[ $# -ge 2 ]] || { echo 'Missing profile' >&2; exit 2; }; profile="$2"; shift 2 ;;
    -h|--help) echo 'Usage: scripts/run-nearby-sync-demo.sh [--build-only] [--two | --profile NAME]'; exit 0 ;;
    *) echo "Unknown option: $1" >&2; exit 2 ;;
  esac
done
[[ "$profile" =~ ^[a-z0-9-]{1,32}$ ]] || { echo 'Profile must be 1–32 lowercase letters, digits, or hyphens.' >&2; exit 2; }
[[ "$(uname -s)" == Darwin ]] || { echo 'This demo requires macOS.' >&2; exit 1; }
export MACOSX_DEPLOYMENT_TARGET=12.0
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target}"
# A relative override means relative to the repository, consistently for all tools.
[[ "$CARGO_TARGET_DIR" == /* ]] || export CARGO_TARGET_DIR="$root/$CARGO_TARGET_DIR"
export RUSTC="${RUSTC:-$(rustup which rustc)}"
case "$(uname -m)" in
  arm64) rust_target=aarch64-apple-darwin ;;
  x86_64) rust_target=x86_64-apple-darwin ;;
  *) echo 'Unsupported Mac architecture.' >&2; exit 1 ;;
esac
out="$CARGO_TARGET_DIR/nearby-sync-demo"
stage="$out/stage"
mkdir -p "$stage/bindings/swift" "$stage/demos/NearbySyncMac"
# Stage the exact local SDK sources so the demo keeps its documented relative
# SwiftPM dependency without replacing a consumer's universal/iOS XCFramework.
rsync -a --delete --exclude '.build' --exclude 'LibreSyncFFI.xcframework' "$root/bindings/swift/" "$stage/bindings/swift/"
rsync -a --delete --exclude '.build' "$root/demos/NearbySyncMac/" "$stage/demos/NearbySyncMac/"
(cd "$root" && cargo build --locked -p libresync-ffi --target "$rust_target")
framework="$stage/bindings/swift/LibreSyncFFI.xcframework"
rm -rf "$framework"
xcodebuild -create-xcframework -library "$CARGO_TARGET_DIR/$rust_target/debug/liblibresync_ffi.a" -output "$framework"
swift build --package-path "$stage/demos/NearbySyncMac" --scratch-path "$out/swift-build" -c debug
bin="$(swift build --package-path "$stage/demos/NearbySyncMac" --scratch-path "$out/swift-build" -c debug --show-bin-path)"
app="$out/Nearby Notes.app"
mkdir -p "$app/Contents/MacOS"
cp "$bin/NearbySyncMac" "$app/Contents/MacOS/NearbySyncMac"
cat > "$app/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>io.libresync.NearbySyncDemo</string>
<key>CFBundleName</key><string>Nearby Notes</string>
<key>CFBundleDisplayName</key><string>Nearby Notes</string>
<key>CFBundleExecutable</key><string>NearbySyncMac</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>1.0</string>
<key>CFBundleVersion</key><string>1</string>
<key>LSMinimumSystemVersion</key><string>12.0</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSLocalNetworkUsageDescription</key><string>Find and sync notes directly with devices you connect on your local network.</string>
<key>NSBonjourServices</key><array><string>_libresync._tcp</string></array>
</dict></plist>
PLIST
codesign --force --sign - --identifier io.libresync.NearbySyncDemo "$app"
codesign --verify --strict "$app"
echo "App: $app"
echo "Staged package: $stage/demos/NearbySyncMac"
echo "Native FFI: $framework"
echo "Tests: swift test --package-path \"$stage/demos/NearbySyncMac\" --scratch-path \"$out/swift-build\""
if ! $build_only; then
  if $two; then
    open -n "$app" --args --profile primary
    open -n "$app" --args --profile secondary
  else
    open -n "$app" --args --profile "$profile"
  fi
fi
