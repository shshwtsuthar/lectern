# Lectern

Lectern is a fast, native-feeling library manager for people who own ebooks. The current thin
application is deliberately focused: it imports EPUBs into a local library, makes them immediately
searchable, renders a cover grid, and edits their metadata.

## What works

- Add individual EPUBs, recursively import a folder, or drop either onto the window.
- Extract title, authors, series, publisher, language, description, and a bounded cover thumbnail.
- Search title, author, series, and publisher with SQLite FTS5 prefix indexes.
- Filter by format and sort by title, author, or recently added.
- Browse a virtualized grid whose cover I/O, image decoding, and database queries stay off the UI
  thread.
- Edit metadata in place and refresh the search index as soon as it is saved.

Bulk editing, device export, filesystem export, and Calibre-library import are not implemented yet.
They remain product work rather than hidden placeholders in this release.

## Quick start

Install Rust with [rustup](https://rustup.rs/). The workspace supports Rust 1.97 or newer and uses
the latest stable toolchain for day-to-day development.

```sh
cargo run
cargo run -p lectern-cli -- --help
cargo test-all
cargo clippy-all
cargo fmt --all --check
```

For a normal optimized build, run `cargo run --release`. Lectern stores its database in the
platform application-data directory. Set `LECTERN_DATA_DIR` to use an explicit location during
development or testing:

```sh
LECTERN_DATA_DIR=/path/to/lectern-data cargo run --release
```

The first import can be started with **Add books**, **Add folder**, or native drag-and-drop. Click a
book card to open its metadata editor; `Ctrl-S` on Windows/Linux or `Cmd-S` on macOS saves changes.

## Workspace

```text
crates/
├── lectern-core/     # UI- and infrastructure-independent application boundary
├── lectern-desktop/  # Native application
├── lectern-import/   # EPUB discovery and ingestion
├── lectern-storage/  # SQLite persistence
└── lectern-cli/      # Command-line diagnostics and automation
docs/
└── adr/           # Architecture decision records
```

Shared dependency versions live at the workspace root, while adapter-specific dependencies stay in
their owning crates. The desktop uses egui/eframe, storage uses bundled SQLite through rusqlite, and
EPUB ingestion uses bounded ZIP, XML, and image processing.

The application has performance-conscious boundaries but makes no unmeasured performance claims.
Once the thin workflow and representative libraries are stable, profiling and targeted benchmarks
can guide optimization of cold launch, import throughput, query latency, scrolling, and memory.

## Quality gates

Every pull request is expected to pass formatting, Clippy with warnings denied, tests on Linux,
macOS, and Windows, documentation checks, an MSRV build, and the dependency policy in `deny.toml`.
CI definitions live in `.github/workflows/ci.yml`.

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development workflow and
[docs/architecture.md](docs/architecture.md) for dependency rules.

## License

No distribution license has been selected yet. Add one before distributing the project outside
its authorized contributors.
