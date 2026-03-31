# Release engineering design

## Goal

Establish a repeatable release lane for LibreSync that keeps versions, release notes, CI checks, and packaging validation aligned without introducing a heavy publishing platform. This track should close the current gap where workspace manifests are already `0.3.0` while formal release notes still stop at `v0.2.0`.

## Scope

This design covers:

- Version alignment across workspace crates, the AlwaysOn Tauri app, and release notes.
- A documented release workflow for maintainers.
- CI checks that catch version drift and run packaging smoke tests.
- Local scripts that keep release verification easy to run by contributors.

This design does not cover:

- Signed installers, notarization, or store distribution.
- Automatic GitHub release publishing.
- Cross-platform artifact hosting.

## Recommended approach

Adopt a lightweight release-ready lane:

1. Treat manifest versions plus release notes as a coordinated release surface.
2. Add one release-prep script that validates the current repo state.
3. Expand CI to run the same validation plus a Tauri packaging smoke test.
4. Document the release workflow in repository docs.

This keeps the process small enough for the current repo while removing the biggest sources of drift and manual error.

## Architecture

### Release source of truth

The source of truth remains distributed but explicitly checked:

- Rust crate versions in workspace manifests.
- Tauri app version in `alwaysOn/libresync-always-on/src-tauri/tauri.conf.json`.
- Current release entry in `RELEASES.md`.

Instead of introducing a new manifest file, LibreSync will enforce consistency by script. That fits the current repository style and avoids creating a second system maintainers must remember to update.

### Release validation script

Add a script under `scripts/utilities/` that validates:

- All expected crate versions match.
- Tauri app version matches the crate version.
- `RELEASES.md` contains an entry for the current version.
- The working tree can pass standard release checks when run locally.

The script should fail clearly and print actionable messages when any check is out of sync.

### CI expansion

Keep the current `cargo test` and coverage job, then add release-focused checks:

- Run the release validation script.
- Build the Tauri app in a smoke-test mode to catch packaging regressions.
- Keep the CI shape simple so local and CI commands stay close.

The goal is not to produce distributable binaries yet. The goal is to prove the repo still packages cleanly.

### Maintainer workflow

Document a simple manual release sequence:

1. Update versions if needed.
2. Add or update the matching `RELEASES.md` entry.
3. Run the release validation script.
4. Run tests and packaging smoke checks.
5. Tag and publish through the maintainer’s normal Git workflow.

This gives the project a clear path for `v0.3.0` and future releases without requiring secrets or new infrastructure now.

## Components

### 1. `RELEASES.md` update

Add a `v0.3.0` entry that summarizes the work already present in the repo. This resolves the biggest user-facing inconsistency immediately.

### 2. Release workflow documentation

Add a short release process document under `docs/`, linked from `README.md` and `CONTRIBUTING.md`, describing:

- Release prerequisites
- Commands to run
- Required file updates
- What counts as release-ready

### 3. Release validation script

Add a shell script that contributors and CI can both run. The script should:

- Read versions from expected files
- Compare them against the core crate version
- Verify `RELEASES.md` includes the matching heading
- Exit non-zero on drift

### 4. CI workflow updates

Extend `.github/workflows/ci.yml` with:

- A release-consistency step
- A Tauri packaging smoke-test step

If the Tauri smoke test needs extra Linux packages, install only the minimum required dependencies in the workflow.

## Data flow

The release data flow is simple:

1. Maintainer updates versioned files and release notes.
2. Validation script reads manifest versions and release notes.
3. CI reruns the same validation.
4. Packaging smoke test confirms the AlwaysOn app still builds.

This creates one feedback loop shared by local development and CI.

## Error handling

The validation script should prefer direct, friendly failures, for example:

- Which file has the mismatched version
- Which release heading is missing
- Which command to run next

The CI workflow should keep these checks as separate named steps so failures are easy to diagnose from GitHub Actions logs.

## Testing strategy

Validation for this track consists of:

- Running the release validation script locally
- Running `cargo test`
- Running the updated CI steps in the local repo where practical
- Verifying the Tauri packaging smoke test command succeeds

Because this track is mostly tooling and docs, confidence comes from command-level verification rather than unit tests alone.

## Risks and mitigations

### Risk: Tauri packaging adds heavy CI complexity

Mitigation:

- Start with a Linux-only smoke build in CI.
- Avoid artifact publishing and signing in this track.

### Risk: Release notes become hand-maintained drift again

Mitigation:

- Enforce presence of a release heading for the current version in the validation script.

### Risk: New scripts duplicate existing contributor guidance

Mitigation:

- Keep release instructions short and link to existing testing/security docs instead of repeating them.

## Deliverables

- `RELEASES.md` updated with `v0.3.0`
- New release process doc in `docs/`
- New release validation script in `scripts/utilities/`
- CI workflow expanded for release consistency and packaging smoke checks
- README and contributor docs updated to point at the release flow

## Success criteria

Track 1 is complete when:

- Repo versions and release notes are aligned on `0.3.0`
- Contributors have one documented release checklist
- CI fails on version drift
- CI verifies the AlwaysOn app still packages on Linux

## Follow-on tracks

This release lane prepares the next work:

- Track 2 can add deeper product changes without leaving release hygiene behind.
- Track 3 can add platform packaging and runtime work on top of an existing validation path.
