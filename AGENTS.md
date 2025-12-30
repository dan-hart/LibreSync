# Repository Guidelines

## Project Structure & Module Organization
- This repository contains a Rust workspace plus documentation. Key files live at the root:
  - `README.md` — high-level overview and values.
  - `RESEARCH.md` — design and architecture research notes.
  - `PROGRESS.md` — ongoing implementation status; consult during early development.
  - `SECURITY.md`, `PRIVACY.md`, `LICENSE` — security, privacy, and licensing details.
- `docs/` contains architecture and CLI documentation.
- `crates/` contains the Rust crates (`libresync` core and `libresync-cli`).
- `scripts/` contains local security utilities (pre-commit and audit helpers).

## Build, Test, and Development Commands
- Build: `cargo build`
- Tests: `cargo test`

## Coding Style & Naming Conventions
- Use consistent Markdown formatting with short paragraphs and bullet lists.
- Prefer sentence-case headings (as in `RESEARCH.md`).
- File names are uppercase for policy docs (e.g., `SECURITY.md`), and title-case headings within documents.

## Testing Guidelines
- Use `cargo test` for unit and integration tests.

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
