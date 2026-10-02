#!/bin/sh
# Verify the opt-in patch against the actual, unchanged Momentum source commit.
set -eu
momentum_source=${1:?usage: verify-momentum-migration.sh MOMENTUM_REPOSITORY}
libresync_source=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
verification_root=$(mktemp -d "${TMPDIR:-/tmp}/libresync-momentum-managed.XXXXXX")
trap 'rm -rf "$verification_root"' EXIT HUP INT TERM
mkdir "$verification_root/momentum"
git -C "$momentum_source" archive 766064bfa340a391d6a9465b60a030e1dc0c9f95 | tar -x -C "$verification_root/momentum"
(cd "$verification_root/momentum" && git apply --check "$libresync_source/examples/momentum-migration/managed.patch" && git apply "$libresync_source/examples/momentum-migration/managed.patch")
# Keep the archive's existing dependency location; point only this temporary
# source tree at the SDK under verification. No source repository is modified.
ln -s "$libresync_source" "$verification_root/momentum/libresync-src"
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$verification_root/target}" cargo test --manifest-path "$verification_root/momentum/Cargo.toml" -p sp-p2p --features managed --test managed
