#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target}"
export RUSTC="${RUSTC:-$(rustup which rustc)}"
android_sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"
: "${android_sdk:?Set ANDROID_HOME to your SDK}"
ndk="${ANDROID_NDK_HOME:-$android_sdk/ndk/27.2.12479018}"
case "$(uname -s)" in Darwin) host=darwin-x86_64;;Linux) host=linux-x86_64;;*) echo "Unsupported build host" >&2;exit 1;;esac
llvm="$ndk/toolchains/llvm/prebuilt/$host/bin"
export RUSTFLAGS="${RUSTFLAGS:-} -Clink-arg=-Wl,-z,max-page-size=16384"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$llvm/aarch64-linux-android24-clang"
export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$llvm/x86_64-linux-android24-clang"
export AR_aarch64_linux_android="$llvm/llvm-ar" AR_x86_64_linux_android="$llvm/llvm-ar"
for target in aarch64-linux-android x86_64-linux-android;do
 export CC="$llvm/aarch64-linux-android24-clang"
 abi=arm64-v8a
 if [[ "$target" == x86_64-linux-android ]];then export CC="$llvm/x86_64-linux-android24-clang";abi=x86_64;fi
 cargo build -p libresync-ffi --release --target "$target"
 out="bindings/kotlin/src/main/jniLibs/$abi"
 mkdir -p "$out"
 cp "$CARGO_TARGET_DIR/$target/release/liblibresync_ffi.so" "$out/"
 "$llvm/llvm-readelf" -lW "$out/liblibresync_ffi.so" | awk '/LOAD/ {if ($NF!="0x4000" && $NF!="0x10000") exit 1}'
done
"$root/bindings/kotlin/gradlew" -p "$root/bindings/kotlin" assembleRelease :compose:assembleRelease publishReleasePublicationToMavenRepository :compose:publishReleasePublicationToMavenRepository
