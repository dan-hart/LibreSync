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

printf 'release readiness: PASS (%s)\n' "$core_version"
