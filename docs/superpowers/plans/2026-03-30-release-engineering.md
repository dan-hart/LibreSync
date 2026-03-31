# Release Engineering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Align LibreSync on `0.3.0` and add a repeatable release-validation lane covering version consistency, release notes, documentation, CI, and the AlwaysOn desktop smoke build.

**Architecture:** Keep version metadata in existing manifests and `RELEASES.md`, then enforce consistency with one shared shell script used by contributors and CI. Enable direct `cargo check --manifest-path` for the nested Tauri crate with the least-invasive Cargo fix, keeping the release lane independent from the root workspace test path.

**Tech Stack:** Rust workspace manifests, Tauri app manifest, GitHub Actions, POSIX shell, Markdown docs.

---

## File structure

- Modify: `alwaysOn/libresync-always-on/src-tauri/Cargo.toml`
  Responsibility: add the least-invasive Cargo workspace fix so manifest-path checks work cleanly for the nested Tauri crate.
- Modify: `RELEASES.md`
  Responsibility: add the missing `v0.3.0` release entry.
- Modify: `README.md`
  Responsibility: link to the release process doc in the main project docs list.
- Modify: `CONTRIBUTING.md`
  Responsibility: point contributors to the release workflow and validation command.
- Modify: `.github/workflows/ci.yml`
  Responsibility: run release validation and AlwaysOn smoke build in CI.
- Create: `docs/RELEASING.md`
  Responsibility: document the manual release workflow and required checks.
- Create: `scripts/utilities/check-release-readiness.sh`
  Responsibility: validate exact version consistency and latest release-note heading semantics.

### Task 1: Add the missing `v0.3.0` release notes

**Files:**
- Modify: `RELEASES.md`

- [ ] **Step 1: Review current release and recent repo history**

Run:
```bash
git log --oneline --decorate -10
git log -1 --date=short --pretty=format:'%ad' 02471aa
rg -n "overlay|Tailscale|Headscale|custom logical merge|SDK updates|0\\.3\\.0" README.md PROGRESS.md research/session-logs -S
sed -n '1,120p' RELEASES.md
```
Expected: recent commits and docs provide the source material for the `v0.3.0` date and bullet summary.

- [ ] **Step 2: Write the `v0.3.0` release entry**

Add a new top entry derived from the sourced inputs in Step 1. The entry must:
- use the commit date from the `0.3.0` bump commit
- summarize only repo state supported by commit history or docs
- appear above `v0.2.0`

Template:
```md
## v0.3.0 (<date-from-step-1>)
- <bullet derived from recent feature/docs evidence>
- <bullet derived from recent feature/docs evidence>
- <bullet derived from recent feature/docs evidence>
```

- [ ] **Step 3: Sanity-check the release notes**

Run:
```bash
sed -n '1,80p' RELEASES.md
```
Expected: `v0.3.0` appears above `v0.2.0` and reads cleanly.

### Task 2: Document the release workflow

**Files:**
- Create: `docs/RELEASING.md`
- Modify: `README.md`
- Modify: `CONTRIBUTING.md`

- [ ] **Step 1: Write the release guide**

Create `docs/RELEASING.md` with:
- Purpose and scope of the release process
- Files that must stay aligned
- Release command checklist with exact commands:
  - `./scripts/utilities/check-release-readiness.sh`
  - `cargo test`
  - `cargo llvm-cov --workspace --summary-only --fail-under-regions 75`
  - `cargo check --manifest-path alwaysOn/libresync-always-on/src-tauri/Cargo.toml`
  - `git status --short`
  - `git tag vX.Y.Z`
  - `git push origin vX.Y.Z`
- Release semantics:
  - release note headings use `## vX.Y.Z`
  - the current release entry must be the topmost versioned heading
  - the working tree must be clean before creating the tag
- Notes on the AlwaysOn smoke build

- [ ] **Step 2: Link the release guide from the README**

Add one bullet under “Additional documents”:
```md
- [Releasing](docs/RELEASING.md)
```

- [ ] **Step 3: Link the release guide from contributing docs**

Add a short “Releases” section:
```md
## Releases
- Keep crate manifests, the AlwaysOn app version, and `RELEASES.md` aligned.
- Run `./scripts/utilities/check-release-readiness.sh` before cutting a release.
- Follow `docs/RELEASING.md` for the full release checklist.
```

- [ ] **Step 4: Review the doc changes**

Run:
```bash
sed -n '1,220p' docs/RELEASING.md
sed -n '130,190p' README.md
sed -n '1,220p' CONTRIBUTING.md
```
Expected: docs are short, consistent, and link the same release flow.

### Task 3: Add release-readiness validation

**Files:**
- Create: `scripts/utilities/check-release-readiness.sh`

- [ ] **Step 1: Write the validation script**

Create a shell script that:
- reads the version from `crates/libresync/Cargo.toml`
- compares it with:
  - `crates/libresync-cli/Cargo.toml`
  - `crates/libresync-ffi/Cargo.toml`
  - `crates/libresync-alwayson-daemon/Cargo.toml`
  - `alwaysOn/libresync-always-on/src-tauri/Cargo.toml`
  - `alwaysOn/libresync-always-on/src-tauri/tauri.conf.json`
- verifies `RELEASES.md` contains `## v<version>`
- verifies `## v<version>` is the first versioned heading in `RELEASES.md`
- prints a success line on pass

Keep the script metadata-only. It should not run tests, coverage, or the AlwaysOn smoke build. Use simple `sed`/`grep` parsing and fail fast with helpful messages.

- [ ] **Step 2: Make the script executable**

Run:
```bash
chmod +x scripts/utilities/check-release-readiness.sh
```
Expected: file mode is executable.

- [ ] **Step 3: Run the script**

Run:
```bash
./scripts/utilities/check-release-readiness.sh
```
Expected: PASS message confirming all versioned files and release notes align.

### Task 4: Make the AlwaysOn smoke build runnable from the repo root

**Files:**
- Modify: `alwaysOn/libresync-always-on/src-tauri/Cargo.toml`

- [ ] **Step 1: Apply the least-invasive Cargo fix for the nested Tauri crate**

Add an empty workspace table to the nested manifest:
```toml
[workspace]
```

Reasoning: Cargo already suggests this as one valid fix, and it avoids changing the root workspace topology just to support the smoke-build command.

- [ ] **Step 2: Verify the targeted Tauri check now resolves**

Run:
```bash
cargo check --manifest-path alwaysOn/libresync-always-on/src-tauri/Cargo.toml
```
Expected: workspace resolution error is gone. If system libraries are missing, Cargo should fail later with a dependency/toolchain message instead.

### Task 5: Expand CI for release readiness and AlwaysOn smoke build

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Add a release-readiness step**

Insert a step after checkout/tooling:
```yaml
      - name: Release readiness
        run: ./scripts/utilities/check-release-readiness.sh
```

- [ ] **Step 2: Add Linux packages needed for the Tauri smoke build**

Pin the job to `ubuntu-22.04` and install the minimum required dependencies before the smoke build:
```yaml
    runs-on: ubuntu-22.04
      - name: Install AlwaysOn build dependencies
        run: |
          sudo apt-get update
          sudo apt-get install -y libwebkit2gtk-4.0-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev
```

- [ ] **Step 3: Add the AlwaysOn smoke build step**

Run:
```yaml
      - name: AlwaysOn smoke build
        run: cargo check --manifest-path alwaysOn/libresync-always-on/src-tauri/Cargo.toml
```

- [ ] **Step 4: Validate workflow syntax by inspection**

Run:
```bash
sed -n '1,240p' .github/workflows/ci.yml
```
Expected: CI still runs tests and coverage, plus the new release and AlwaysOn checks.

### Task 6: Full verification

**Files:**
- N/A

- [ ] **Step 1: Run release validation**

Run:
```bash
./scripts/utilities/check-release-readiness.sh
```
Expected: PASS

- [ ] **Step 2: Run workspace tests**

Run:
```bash
cargo test
```
Expected: PASS

- [ ] **Step 3: Run the local AlwaysOn smoke build**

Run:
```bash
cargo check --manifest-path alwaysOn/libresync-always-on/src-tauri/Cargo.toml
```
Expected: PASS, or a clearly identified environment-specific system dependency issue that is documented in the final response.

- [ ] **Step 4: Summarize verification results**

Record:
- whether release validation passed
- whether workspace tests passed
- whether the AlwaysOn smoke build passed locally
- whether the release heading is topmost and matches the manifest version

- [ ] **Step 5: Commit the release-engineering implementation**

Run:
```bash
git add alwaysOn/libresync-always-on/src-tauri/Cargo.toml RELEASES.md README.md CONTRIBUTING.md docs/RELEASING.md scripts/utilities/check-release-readiness.sh .github/workflows/ci.yml
git commit -m "Add release readiness checks and docs"
```
