# Lectern

Lectern is a fast, native-feeling library manager for people who own ebooks. The current thin
application is deliberately focused: it imports EPUB and PDF books into a local library, makes them
immediately searchable, renders a cover grid, and edits their metadata.

## What works

- Add individual EPUB or PDF books, recursively import a folder, or drop either onto the window.
- Extract EPUB metadata and embedded covers; extract standard PDF metadata and render the first page
  as a bounded cover thumbnail.
- Search with safe fielded prefixes/phrases and combine exact contributor, series, tag, format, and
  file-health filters.
- Represent one logical book with one or more stable file assets and filter by any available format.
- Sort by title, author, series, or recently added.
- Browse a virtualized grid backed by bounded result pages; cover I/O, image decoding, and database
  queries stay off the UI thread.
- Edit ordered contributor roles, contributor sort names, series membership and decimal book
  number, and flat tags in place; normalized identities and the search index update atomically.
- Select individual books, unloaded-page ranges, or every current result, then add and remove tags
  atomically with exact affected counts and bounded memory.
- Rename, merge, and delete contributors, series, and tags from a bounded, searchable organiser.
- Save, apply, explicitly update, rename, and delete complete query/filter/sort projections.
- Attach a missing EPUB or PDF format to an existing book after validating the selected file,
  without changing its metadata, cover, or existing file assets.
- Remove a book from the library without deleting any of its EPUB or PDF files.
- Rescan referenced book files, filter missing or unreadable assets, and safely relink a missing
  EPUB or PDF without losing its logical-book metadata, cover, or asset identity.
- Run library diagnostics and create a consistent, validated SQLite backup from the command line,
  including while the live library has committed data in its WAL.

Device export, filesystem export, and Calibre-library import are not implemented yet.
Password-protected PDFs also require a future password prompt. These remain product work rather
than hidden placeholders in this release.

The next file-management tranche adds detach, open, reveal, deliberate replacement, and
single-asset export without adding another parser or deleting externally referenced files. Its
rules, sequence, performance evidence, and exclusions are defined in
[`docs/asset-management-scope.md`](docs/asset-management-scope.md).

The target GPUI interface will use an internal, token-generated Primer design-system layer. It is a
one-component-at-a-time application architecture rather than a general Primer port; see the
[Primer-to-GPUI porting guide](docs/porting-primer-to-gpui.md) and
[ADR 0004](docs/adr/0004-own-a-primer-inspired-gpui-ui-layer.md). The production desktop remains on
egui/eframe while the additive `lectern-gpui` executable carries the first migrated empty-library
slice.

The first broad library-management slice is **Organisation**: normalized contributors and series,
flat tags, safe multi-selection and bulk tagging, exact filters, fielded search, and saved searches.
Its end-to-end workflow, data contracts, query grammar, budgets, and implementation sequence are
defined in [`docs/organisation-scope.md`](docs/organisation-scope.md).

## Quick start

Install Rust with [rustup](https://rustup.rs/). The workspace supports Rust 1.97 or newer and uses
the latest stable toolchain for day-to-day development.

```sh
cargo run
cargo run --release -p lectern-desktop --bin lectern-gpui
cargo run -p lectern-ui --example component_gallery
cargo run -p lectern-cli -- stats
cargo run -p lectern-cli -- doctor
cargo run -p lectern-cli -- backup /path/to/library-backup.sqlite3
cargo run -p lectern-cli -- import /path/to/books /path/to/another.epub
cargo run -p lectern-cli -- scan
cargo test-all
cargo clippy-all
cargo fmt --all --check
```

On Debian or Ubuntu, the GPUI executable additionally needs the XKB development libraries:
`libxkbcommon-dev` and `libxkbcommon-x11-dev`.

For a normal optimized build, run `cargo run --release`. Lectern stores its database in the
platform application-data directory. Set `LECTERN_DATA_DIR` to use an explicit location during
development or testing:

```sh
LECTERN_DATA_DIR=/path/to/lectern-data cargo run --release
cargo run -p lectern-cli -- --database /path/to/library.sqlite3 doctor
```

Administrative CLI commands require an existing library and do not silently create one. Backups
are online SQLite snapshots, include committed WAL data, validate integrity and book counts before
publication, and refuse to overwrite an existing destination.

The first import can be started with **Add books**, **Add folder**, or native drag-and-drop. Click a
book card to edit its metadata, attach another format, or remove it from Lectern while keeping its
original files; `Ctrl-S` on Windows/Linux or `Cmd-S` on macOS saves changes. Use `Ctrl`/`Cmd`-click
to toggle grid selection, `Shift`-click for a range, and the toolbar to select all matching books,
edit their tags, manage exact filters, or reuse a saved search.

## Workspace

```text
crates/
├── lectern-core/     # Domain language and workflow contract
├── lectern-service/  # Application policy and workflow orchestration
├── lectern-desktop/  # Native application
├── lectern-ui/       # Generated Primer tokens and native GPUI components
├── lectern-import/   # EPUB and PDF discovery and ingestion
├── lectern-storage/  # SQLite persistence
├── lectern-cli/      # Command-line diagnostics and automation
└── xtask/            # Deterministic source and asset generation
docs/
└── adr/           # Architecture decision records
```

Shared dependency versions live at the workspace root, while adapter-specific dependencies stay in
their owning crates. The production desktop uses egui/eframe, the additive migration target uses
GPUI and `lectern-ui`, storage uses bundled SQLite through rusqlite, EPUB ingestion uses bounded
ZIP, XML, and image processing, and PDF ingestion uses bounded parsing with a native CPU renderer.

The storage model keeps logical metadata and cover data separate from format-specific file assets.
Current file and folder import remains conservative and does not guess that similarly named files
are the same book; trusted aggregate importers such as the planned Calibre adapter can attach
several formats atomically.

The application has performance-conscious boundaries and deterministic weekly regression suites
for backup and diagnostics, full-result and paged queries, curation, single-book removal, and asset
lifecycle workflows against a 50,000-book library. The broader benchmark study retains raw
measurements for cold launch, import throughput, scrolling, and memory; see
[`benchmarks/README.md`](benchmarks/README.md) for how to run and interpret both workflows.

## Quality gates

Every pull request is expected to pass formatting, Clippy with warnings denied, tests on Linux,
macOS, and Windows, documentation checks, an MSRV build, the benchmark-runner contract tests, and
the dependency policy in `deny.toml`. Performance-sensitive pull requests run three paired
base/candidate release-query measurements and compare their median run-level p95 values. The same
absolute suites run weekly on a pinned GitHub runner and are manually dispatchable. CI definitions
live in `.github/workflows/`.

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development workflow and
[docs/architecture.md](docs/architecture.md) for dependency rules. Performance-sensitive changes
are governed by the mandatory classification, measurement, and merge rules in
[`docs/performance-policy.md`](docs/performance-policy.md).

## License

No distribution license has been selected yet. Add one before distributing the project outside
its authorized contributors.
