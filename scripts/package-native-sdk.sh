#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
version="${1:?Usage: package-native-sdk.sh VERSION [OUTPUT_DIRECTORY]}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "Invalid version" >&2; exit 1; }
out="${2:-${CARGO_TARGET_DIR:-$root/target}/native-sdk/$version}"
mkdir -p "$out"
# zip updates existing archives; delete only this version's generated assets first.
rm -f "$out/LibreSyncSwift-$version.zip" "$out/LibreSyncFFI-$version.xcframework.zip" "$out/LibreSyncAndroid-$version-maven.zip"
out="$(cd "$out" && pwd)"
if [[ "${SKIP_BUILD:-0}" != 1 ]];then "$root/scripts/build-apple-sdk.sh";"$root/scripts/build-android-sdk.sh";fi
stage="$(mktemp -d "${TMPDIR:-/tmp}/libresync-sdk.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/LibreSyncSwift-$version"
for item in Package.swift Sources Tests Samples README.md LibreSyncFFI.xcframework;do cp -R "$root/bindings/swift/$item" "$stage/LibreSyncSwift-$version/";done
cp "$root/LICENSE" "$stage/LibreSyncSwift-$version/LICENSE"
(cd "$stage" && zip -qr "$out/LibreSyncSwift-$version.zip" "LibreSyncSwift-$version")
(cd "$root/bindings/swift" && zip -qr "$out/LibreSyncFFI-$version.xcframework.zip" LibreSyncFFI.xcframework)
zip -jq "$out/LibreSyncFFI-$version.xcframework.zip" "$root/LICENSE"
cp "$root/bindings/kotlin/build/outputs/aar/LibreSyncAndroid-release.aar" "$out/LibreSyncAndroid-$version.aar"
cp "$root/bindings/kotlin/compose/build/outputs/aar/compose-release.aar" "$out/LibreSyncCompose-$version.aar"
(cd "$root/bindings/kotlin/build/repository" && zip -qr "$out/LibreSyncAndroid-$version-maven.zip" .)
zip -jq "$out/LibreSyncAndroid-$version-maven.zip" "$root/LICENSE"
cp "$root/bindings/managed-v1.schema.json" "$root/bindings/include/libresync.h" "$out/"
(cd "$out" && shasum -a 256 ./*.zip ./*.aar > SHA256SUMS)
# Install the generated root manifest before tagging. CI/source builds explicitly
# select the local framework; Git consumers default to the immutable release bytes.
checksum="$(swift package compute-checksum "$out/LibreSyncFFI-$version.xcframework.zip")"
python3 - "$root" "$out" "$version" "$checksum" <<'PY'
import sys
from pathlib import Path
root,out,version,checksum=sys.argv[1:]
s=(Path(root)/'bindings/swift/Package.swift').read_text()
s=s[s.index('// swift-tools-version:'):]
comment_start=s.index('// The Swift package')
comment_end=s.index('let package = Package(')
s=s[:comment_start]+'// Git consumers use exact immutable release bytes; source/CI opts into a built local framework.\n'+s[comment_end:]
s=s.replace('import PackageDescription', 'import PackageDescription\nimport Foundation')
s=s.replace('let package = Package(', f'''let nativeBinary: Target = ProcessInfo.processInfo.environment["LIBRESYNC_USE_LOCAL_XCFRAMEWORK"] == "1"\n    ? .binaryTarget(name: "LibreSyncFFI", path: "bindings/swift/LibreSyncFFI.xcframework")\n    : .binaryTarget(name: "LibreSyncFFI", url: "https://github.com/dan-hart/LibreSync/releases/download/v{version}/LibreSyncFFI-{version}.xcframework.zip", checksum: "{checksum}")\n\nlet package = Package(''')
start=s.index('        .binaryTarget(')
end=s.index('        .target(',start)
s=s[:start]+'        nativeBinary,\n'+s[end:]
s=s.replace('path: "Sources/', 'path: "bindings/swift/Sources/').replace('path: "Tests/', 'path: "bindings/swift/Tests/')
(Path(out)/'Package.release.swift').write_text(s)
PY
printf 'SDK artifacts: %s\n' "$out"
