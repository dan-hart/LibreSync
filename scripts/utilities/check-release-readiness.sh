#!/usr/bin/env sh

set -eu

script_dir=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)
cd "$repo_root"

fail() {
  printf 'release readiness: FAIL: %s\n' "$1" >&2
  exit 1
}

read_cargo_version() {
  file=$1
  version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$file" | head -n 1)
  [ -n "$version" ] || fail "could not read version from $file"
  printf '%s\n' "$version"
}

read_tauri_conf_version() {
  file=$1
  version=$(sed -n 's/.*"version":[[:space:]]*"\([^"]*\)".*/\1/p' "$file" | head -n 1)
  [ -n "$version" ] || fail "could not read version from $file"
  printf '%s\n' "$version"
}

check_matches() {
  file=$1
  actual=$2
  expected=$3
  [ "$actual" = "$expected" ] || fail "$file has version $actual but expected $expected"
}

core_version=$(read_cargo_version "crates/libresync/Cargo.toml")

check_matches "crates/libresync-cli/Cargo.toml" \
  "$(read_cargo_version "crates/libresync-cli/Cargo.toml")" \
  "$core_version"
check_matches "crates/libresync-ffi/Cargo.toml" \
  "$(read_cargo_version "crates/libresync-ffi/Cargo.toml")" \
  "$core_version"
check_matches "crates/libresync-alwayson-daemon/Cargo.toml" \
  "$(read_cargo_version "crates/libresync-alwayson-daemon/Cargo.toml")" \
  "$core_version"
check_matches "alwaysOn/libresync-always-on/src-tauri/Cargo.toml" \
  "$(read_cargo_version "alwaysOn/libresync-always-on/src-tauri/Cargo.toml")" \
  "$core_version"
check_matches "alwaysOn/libresync-always-on/src-tauri/tauri.conf.json" \
  "$(read_tauri_conf_version "alwaysOn/libresync-always-on/src-tauri/tauri.conf.json")" \
  "$core_version"

expected_heading="## v$core_version"
actual_heading=$(grep '^## v' RELEASES.md | head -n 1 || true)
[ -n "$actual_heading" ] || fail "RELEASES.md has no version headings"
case "$actual_heading" in
  "$expected_heading"*) ;;
  *)
    fail "latest release heading is '$actual_heading' but expected '$expected_heading'"
    ;;
esac

python3 - "$core_version" <<'PYTHON'
import re, sys
from pathlib import Path
version=sys.argv[1]
def require(ok,message):
    if not ok: raise SystemExit("release readiness: FAIL: "+message)
for name in ('bindings/kotlin/build.gradle.kts','bindings/kotlin/compose/build.gradle.kts'):
    require(re.search(r'version\s*=\s*"'+re.escape(version)+r'"',Path(name).read_text()),name+' version mismatch')
require('versionName="'+version+'"' in Path('bindings/kotlin/sample/build.gradle.kts').read_text(),'Android sample version mismatch')
for name in ('Cargo.lock','alwaysOn/libresync-always-on/src-tauri/Cargo.lock'):
    for package,actual in re.findall(r'name = "(libresync[^"\n]*)"\nversion = "([^"\n]+)"',Path(name).read_text()):
        require(actual==version,name+' '+package+' version mismatch')
for name in Path('crates').glob('*/Cargo.toml'):
    for actual in re.findall(r'libresync\s*=.*version\s*=\s*"([^"]+)"',name.read_text()): require(actual==version,str(name)+' dependency mismatch')
p=Path('Package.swift')
require(p.exists(),'root Swift release manifest missing')
s=p.read_text()
require(f'/download/v{version}/LibreSyncFFI-{version}.xcframework.zip' in s,'root Swift asset version mismatch')
require(re.search(r'checksum: "[0-9a-f]{64}"',s),'root Swift checksum missing')
require('LIBRESYNC_USE_LOCAL_XCFRAMEWORK' in s,'explicit source-build selection missing')
# Optional exact-byte verification for the release operator; CI needs no unpublished assets.
import os,hashlib
if directory:=os.environ.get('LIBRESYNC_SDK_ARTIFACT_DIR'):
    asset=Path(directory)/f'LibreSyncFFI-{version}.xcframework.zip'
    require(asset.exists(),'final XCFramework asset missing')
    require(hashlib.sha256(asset.read_bytes()).hexdigest()==re.search(r'checksum: "([0-9a-f]{64})"',s)[1],'root Swift checksum differs from final bytes')
    import zipfile, io
    for filename in (f'LibreSyncSwift-{version}.zip',f'LibreSyncFFI-{version}.xcframework.zip',f'LibreSyncAndroid-{version}-maven.zip',f'LibreSyncAndroid-{version}.aar',f'LibreSyncCompose-{version}.aar'):
        path=Path(directory)/filename
        require(path.exists(),'missing final asset '+filename)
        with zipfile.ZipFile(path) as archive:
            names=archive.namelist()
            licensed=any(name.endswith('LICENSE') or name.endswith('LICENSE.txt') for name in names)
            if not licensed and filename.endswith('.aar') and 'classes.jar' in names:
                with zipfile.ZipFile(io.BytesIO(archive.read('classes.jar'))) as classes:
                    licensed=any(name.endswith('LICENSE') for name in classes.namelist())
            require(licensed,filename+' missing license')
            if filename.endswith('-maven.zip'):
                for artifact in ('libresync','libresync-compose'):
                    name=f'io/libresync/{artifact}/{version}/{artifact}-{version}.pom'
                    require(name in archive.namelist(),filename+' missing publication '+artifact)
                    require(f'<version>{version}</version>' in archive.read(name).decode(),artifact+' publication version mismatch')
PYTHON

printf 'release readiness: PASS (%s)\n' "$core_version"
