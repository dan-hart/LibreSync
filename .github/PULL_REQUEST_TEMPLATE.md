## Summary

## Checklist
- [ ] `cargo test --workspace` and `cargo test -p libresync --all-features` pass
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` is clean
- [ ] `CHANGELOG.md` has an entry under `[Unreleased]` for user-visible changes
- [ ] Docs updated (`README.md`, `docs/`, `SECURITY.md`, `docs/PROTOCOL.md`) where behavior changed
- [ ] No keys, config, state files, or private addresses in the diff

## Security rationale
Required if this touches discovery, linking, encryption, key storage, or the wire format. Otherwise write "n/a".
