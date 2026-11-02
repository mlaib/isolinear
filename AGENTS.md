# Working on Isolinear

Read `CONTRIBUTING.md` and `FORMATS.md` first. Work story by story; do not widen a story.

Principles that every change respects:
- Source bytes are authoritative. Views are projections linked by byte spans; saves are atomic
  and keep encoding, BOM and line endings.
- Nothing is executed, fetched or written except by an explicit user action.
- Parsing and I/O run on workers with cancellation; every parser has explicit limits and tests
  for malformed input.
- Rust owns bytes and structure (`crates/`), the UI owns pixels (`ui/`), Tauri joins them
  (`src-tauri/`).

Verification before a commit: `cargo fmt --all -- --check`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo test --workspace`, `npm run check`, `npm test`, `npm run build`.
A compiling build is not an accepted UI change: run the app on the change.

Commit messages are short, one line when possible, no trailers.
