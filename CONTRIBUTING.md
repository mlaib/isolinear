# Contributing

Thank you for helping. Isolinear is a viewer and editor that must never damage a file, run what a
file contains, or reach the network. Changes are judged first on that.

## Setup

Rust 1.92, Node 22 and the [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/).

```sh
npm ci
npm run tauri dev -- -- path/to/file     # the app, reloading the UI as you edit it
cargo run -p isolinear-cli -- inspect FILE   # what a file is and which views it gets
cargo run -p isolinear-cli -- tree FILE      # the structure tree of a text file
```

## Before a pull request

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npm run check && npm test && npm run build
```

A change to the window is checked in the window, not only by compiling. Say in the pull request what
you opened and what you saw.

## Rules

- Source bytes are authoritative: views never rewrite a file; saving keeps its encoding, BOM and line
  endings.
- Readers never panic on any input, check every length before allocating, and come with tests for
  damaged input and a randomized test. Parsing runs on workers.
- Nothing is executed (scripts, macros, formulas, SQL, pickles) and nothing is fetched.
- New formats: see [Adding a format](#adding-a-format) below.
- Commit messages are short.

## Adding a format

A format is a registry entry plus, depending on what it is, a structure tree, a table reader or a
document converter. Everything lives in `crates/formats`; the window picks it up from there.

### 1. Recognize it

- `src/registry.rs`: a `Spec` with an id, a name, extensions, `Kind::Text` or `Kind::Binary`, the
  views it offers (the first is the one it opens on) and whether it can be edited (text only).
- `src/signature.rs`: its signature, if it has one. A signature wins over the extension.
- `src/detect.rs`: content probes for text formats without a telling extension.

`cargo run -p isolinear-cli -- inspect FILE` shows what detection decided and why.

### 2. Structure (text formats): `src/tree/`

Implement `TreeFormat::parse(source, limits, cancel) -> Tree` with a `Builder`:

- add nodes **in source order**: `open` a container, `leaf` a value, `close` the container;
- spans are byte offsets; a member's span starts at its key; names and values as written;
- recoverable problems are `error` / `warning` / `info` diagnostics; a fatal one is `fail`, then
  `finish`, keeping what was parsed;
- return as soon as the builder reports `Stopped` (a limit or cancellation).

Register it in `tree::for_format`. `src/tree/json.rs` is the reference, tests included.
`cargo run -p isolinear-cli -- tree FILE` prints the tree.

### 3. Tables (binary data): `src/table/`

Implement `TableSource`: `schema()` (tables of the file, columns and types, row count, facts, schema
text) and `rows(start, count)` (display text, `None` for null). Read headers on open and rows only
when asked; check every length against the file size. `src/table/npy.rs` is the reference.

### 4. Tests

Every reader has tests for valid files, for damaged ones (truncated, wrong lengths, loops), for its
limits, and a randomized or mutation test of at least 20,000 inputs. Readers never panic.
Every fixture also goes through `crates/formats/tests/file_safety.rs`, which reads it every way
the window can and fails if a byte or its modification time changes.

### 5. What a reader must never do

Run anything the file contains (scripts, macros, formulas, SQL, pickled objects), read other files
than the one opened (and sidecars the format defines), expand external entities, or use the network.

## Licence

Contributions are licensed under Apache-2.0 (section 5 of the [licence](LICENSE)).
