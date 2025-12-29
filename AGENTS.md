# Repository Guidelines

## Project Structure & Module Organization
- This repository currently contains project documentation only. Key files live at the root:
  - `README.md` — high-level overview and values.
  - `RESEARCH.md` — design and architecture research notes.
  - `SECURITY.md`, `PRIVACY.md`, `LICENSE` — security, privacy, and licensing details.
- `scripts/` contains local security utilities (pre-commit and audit helpers).
- There is no `src/` or `tests/` directory yet; expect these to be added as the Rust core and platform bindings are implemented.

## Build, Test, and Development Commands
- No build or test tooling is defined yet. When code lands, add a `Cargo.toml` and document commands such as:
  - `cargo build` — compile the Rust core.
  - `cargo test` — run unit and integration tests.
- Until then, contributions focus on documentation updates.

## Coding Style & Naming Conventions
- Use consistent Markdown formatting with short paragraphs and bullet lists.
- Prefer sentence-case headings (as in `RESEARCH.md`).
- File names are uppercase for policy docs (e.g., `SECURITY.md`), and title-case headings within documents.

## Testing Guidelines
- No test framework is present yet.
- When tests are added, document the framework (e.g., `cargo test`, `criterion`) and naming conventions (e.g., `tests/*.rs`).

## Commit & Pull Request Guidelines
- Git history is not available in this repo, so no commit convention is established.
- Suggested default: short, imperative commit summaries (e.g., “Add sync protocol outline”).
- PRs should include:
  - A concise description of changes.
  - Links to relevant issues or research sections (e.g., `RESEARCH.md` headings).
  - Screenshots only if visuals are introduced later.

## Security & Configuration Tips
- The project prioritizes local-only sync and privacy; avoid adding cloud dependencies without explicit discussion.
- If adding security-related content, update `SECURITY.md` and call out threat-model impacts.
