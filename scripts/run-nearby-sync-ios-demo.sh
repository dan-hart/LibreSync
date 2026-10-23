#!/usr/bin/env bash
# Build the current source SDK and install the iOS demo on an explicitly named device.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
team="${DEVELOPMENT_TEAM:-}"
device=""
build_only=false
skip_sdk=false
usage() { echo 'Usage: scripts/run-nearby-sync-ios-demo.sh --team TEAM_ID --device DEVICE [--skip-sdk-build] [--build-only]'; }
while [[ $# -gt 0 ]]; do
  case "$1" in
    --team) [[ $# -ge 2 ]] || { usage >&2; exit 2; }; team="$2"; shift 2 ;;
    --device) [[ $# -ge 2 ]] || { usage >&2; exit 2; }; device="$2"; shift 2 ;;
    --build-only) build_only=true; shift ;;
    --skip-sdk-build) skip_sdk=true; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done
[[ "$(uname -s)" == Darwin ]] || { echo 'This demo requires macOS and Xcode.' >&2; exit 1; }
[[ "$team" =~ ^[A-Z0-9]{10}$ ]] || { echo 'Supply your development team with --team or DEVELOPMENT_TEAM.' >&2; exit 2; }
if ! $build_only && [[ -z "$device" ]]; then
  echo 'Supply --device to authorize installation on a specific device.' >&2; exit 2
fi
command -v asc >/dev/null || { echo 'Install the App Store Connect CLI (asc) to use this build helper.' >&2; exit 1; }
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target}"
[[ "$CARGO_TARGET_DIR" == /* ]] || export CARGO_TARGET_DIR="$root/$CARGO_TARGET_DIR"
out="$CARGO_TARGET_DIR/nearby-sync-ios"
mkdir -p "$out"
if ! $skip_sdk; then "$root/scripts/build-apple-sdk.sh" --debug; fi
project="$root/demos/NearbySyncIOS/NearbyNotesIOS.xcodeproj"
[[ -f "$project/project.pbxproj" ]] || { echo 'Generate the iOS project with XcodeGen as described in the demo README.' >&2; exit 1; }
destination='generic/platform=iOS'
if [[ -n "$device" ]]; then
  xcrun devicectl list devices --quiet --json-output "$out/devices.json" --timeout 30
  device_udid="$(python3 - "$out/devices.json" "$device" <<'PYDEVICE'
import json, sys
matches = []
for item in json.load(open(sys.argv[1])).get("result", {}).get("devices", []):
    properties = item.get("properties", {})
    hardware = properties.get("hardware", item.get("hardwareProperties", {}))
    state = properties.get("state", item.get("deviceProperties", {}))
    udid = hardware.get("udid")
    if udid and sys.argv[2] in (item.get("identifier"), state.get("name"), udid):
        if hardware.get("platform") == "iOS" and hardware.get("reality") == "physical":
            matches.append(udid)
if len(matches) != 1:
    raise SystemExit("Expected exactly one named physical iOS device; found " + str(len(matches)))
print(matches[0])
PYDEVICE
)"
  device="$device_udid"
  destination="platform=iOS,id=$device"
fi
asc xcode build --project "$project" --scheme NearbyNotesIOS --configuration Debug \
  --destination "$destination" --derived-data-path "$out/DerivedData" \
  --xcodebuild-flag="DEVELOPMENT_TEAM=$team" \
  --xcodebuild-flag=-allowProvisioningUpdates --output json > "$out/build-result.json"
app="$out/DerivedData/Build/Products/Debug-iphoneos/NearbyNotesIOS.app"
[[ -d "$app" ]] || { echo "Missing built application: $app" >&2; exit 1; }
codesign --verify --deep --strict "$app"
bundle="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app/Info.plist")"
[[ "$bundle" == io.libresync.NearbySyncDemo ]] || { echo 'Unexpected demo bundle identifier; refusing installation.' >&2; exit 1; }
echo "App: $app"
if ! $build_only; then
  xcrun devicectl device install app --device "$device" "$app" --timeout 60 --json-output "$out/install-result.json"
  xcrun devicectl device process launch --device "$device" --terminate-existing "$bundle" --timeout 30 --json-output "$out/launch-result.json"
  xcrun devicectl device info apps --device "$device" --bundle-id "$bundle" --timeout 30 --json-output "$out/installed-app.json"
  xcrun devicectl device info processes --device "$device" --search NearbyNotesIOS --timeout 30 --json-output "$out/running-process.json"
fi
