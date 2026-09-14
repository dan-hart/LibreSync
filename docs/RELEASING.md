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

1. Create a GitHub release from it (`gh release create vX.Y.Z --notes-file ...`
   with the `RELEASES.md` section).
2. Update `Formula/libresync.rb` in `dan-hart/homebrew-tap`: point `url` at
   `https://github.com/dan-hart/LibreSync/archive/refs/tags/vX.Y.Z.tar.gz` and
   set `sha256` to `curl -sL <url> | sha256sum`.

crates.io publishing is not part of the release process yet. The manifests are
ready for it (`cargo publish --workspace --exclude libresync-alwayson-daemon --dry-run`
passes); when the crates are first published, add the step above and the
`cargo install libresync-cli` line to `README.md`.

## Notes

- `./scripts/utilities/check-release-readiness.sh` is metadata-only. It checks version alignment and the topmost release heading, but it does not run tests or build commands.
- The AlwaysOn smoke build depends on the nested Tauri crate being runnable with `cargo check --manifest-path`.
- `git status --short` should print nothing before creating the tag.
