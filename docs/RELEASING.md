# Releasing

LibreSync releases are manual. The goal is to keep versioned files, release notes, and verification commands aligned before creating a tag.

## Release surface

Keep these files aligned on the same version:

- `crates/libresync/Cargo.toml`
- `crates/libresync-cli/Cargo.toml`
- `crates/libresync-ffi/Cargo.toml`
- `crates/libresync-alwayson-daemon/Cargo.toml`
- `alwaysOn/libresync-always-on/src-tauri/Cargo.toml`
- `alwaysOn/libresync-always-on/src-tauri/tauri.conf.json`
- `RELEASES.md`

## Release notes and tags

- Use release headings in the form `## vX.Y.Z`.
- Keep the current release entry as the topmost versioned heading in `RELEASES.md`.
- Use the same `vX.Y.Z` format for the git tag.

## Prerequisites

- Install `cargo-llvm-cov` locally before running the coverage check.
- Install `cargo-audit` locally before running the dependency audit.
- Install the Rust `llvm-tools-preview` component before running `cargo llvm-cov`.
- Install a Swift toolchain before running the Swift bindings build.
- The AlwaysOn smoke build requires a Tauri-capable local environment. On Linux, install `libwebkit2gtk-4.0-dev`, `libgtk-3-dev`, `libayatana-appindicator3-dev`, and `librsvg2-dev`.

## Release checklist

Run these from the repo root:

```bash
./scripts/utilities/check-release-readiness.sh
./scripts/utilities/security-audit.sh
cargo audit
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test
cargo llvm-cov --workspace --summary-only --fail-under-regions 75
cargo check --manifest-path alwaysOn/libresync-always-on/src-tauri/Cargo.toml
swift build --package-path bindings/swift
git status --short
git tag vX.Y.Z
git push origin main vX.Y.Z
```

After pushing the release tag:

1. Create a GitHub release from the tag (`gh release create vX.Y.Z --notes-from-tag`
   or paste the `RELEASES.md` section).
2. Publish or update the Homebrew formula in `dan-hart/homebrew-tap` so that
   `brew install dan-hart/tap/libresync` resolves to the new tarball. Until the
   formula is published, do not advertise the Homebrew install path in `README.md`.

## Notes

- `./scripts/utilities/check-release-readiness.sh` is metadata-only. It checks version alignment and the topmost release heading, but it does not run tests or build commands.
- The AlwaysOn smoke build depends on the nested Tauri crate being runnable with `cargo check --manifest-path`.
- `git status --short` should print nothing before creating the tag.
- The Homebrew formula lives in `dan-hart/homebrew-tap` and should point at the matching `vX.Y.Z` source tarball with an updated SHA256.
