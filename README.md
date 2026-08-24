# Lectern

Lectern is a fast, native-feeling library manager for people who own ebooks. The current thin
application is deliberately focused: it imports EPUB and PDF books into a local library, makes them
immediately searchable, renders a cover grid, and edits their metadata.

## What works

- Add individual EPUB or PDF books, recursively import a folder, or drop either onto the window.
- Extract EPUB metadata and embedded covers; extract standard PDF metadata and render the first page
  as a bounded cover thumbnail.
- Search title, author, series, and publisher with SQLite FTS5 prefix indexes.
- Represent one logical book with one or more stable file assets and filter by any available format.
- Sort by title, author, or recently added.
- Browse a virtualized grid backed by bounded result pages; cover I/O, image decoding, and database
  queries stay off the UI thread.
- Edit metadata in place and refresh the search index as soon as it is saved.
- Rescan referenced book files, filter missing or unreadable assets, and safely relink a missing
  EPUB or PDF without losing its logical-book metadata, cover, or asset identity.

Bulk editing, device export, filesystem export, and Calibre-library import are not implemented yet.
Password-protected PDFs also require a future password prompt. These remain product work rather
than hidden placeholders in this release.

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
├── lectern-import/   # EPUB and PDF discovery and ingestion
├── lectern-storage/  # SQLite persistence
└── lectern-cli/      # Command-line diagnostics and automation
docs/
└── adr/           # Architecture decision records
```

Shared dependency versions live at the workspace root, while adapter-specific dependencies stay in
their owning crates. The desktop uses egui/eframe, storage uses bundled SQLite through rusqlite,
EPUB ingestion uses bounded ZIP, XML, and image processing, and PDF ingestion uses bounded parsing
with a native CPU renderer.

The storage model keeps logical metadata and cover data separate from format-specific file assets.
Current file and folder import remains conservative and does not guess that similarly named files
are the same book; trusted aggregate importers such as the planned Calibre adapter can attach
several formats atomically.

The application has performance-conscious boundaries and deterministic weekly regression suites
for full-result and paged 50,000-book release-query paths. The broader benchmark study retains raw
measurements for cold launch, import throughput, scrolling, and memory; see
[`benchmarks/README.md`](benchmarks/README.md) for how to run and interpret both workflows.

## Quality gates

Every pull request is expected to pass formatting, Clippy with warnings denied, tests on Linux,
macOS, and Windows, documentation checks, an MSRV build, the benchmark-runner contract tests, and
the dependency policy in `deny.toml`. The deterministic release-query regression suites run weekly
on a pinned GitHub runner and are manually dispatchable. CI definitions live in
`.github/workflows/`.

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development workflow and
[docs/architecture.md](docs/architecture.md) for dependency rules. Performance-sensitive changes
are governed by the mandatory classification, measurement, and merge rules in
[`docs/performance-policy.md`](docs/performance-policy.md).

## License

No distribution license has been selected yet. Add one before distributing the project outside
its authorized contributors.
