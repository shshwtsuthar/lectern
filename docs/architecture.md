# Architecture

Lectern uses a Cargo workspace so the native application can compose independently testable domain,
storage, and import layers.

## Dependency direction

```text
lectern-desktop ────────> lectern-core
       │                       ▲
       ├──> lectern-storage ───┘
       └──> lectern-import ────┬──> lectern-core
                               └──> lectern-storage
```

`lectern-core` owns domain language, use cases, and the interfaces those use cases need. It must not
depend on a desktop framework, database driver, device SDK, network client, or platform API. Adapters
may depend on the core and implement its interfaces; the core must never depend on adapters.

The CLI exists as a lightweight diagnostic and automation surface. It is not a commitment to a
CLI-first product.

## Implemented boundaries

- `lectern-core` owns book, query, format, and sort types without infrastructure dependencies.
- `lectern-storage` owns schema migration, transactional writes, FTS5 queries, and cover blobs.
- `lectern-import` owns EPUB/PDF discovery, bounded parsing and cover rendering, and batched
  ingestion.
- `lectern-desktop` owns native presentation, worker coordination, and platform dialogs.
- `lectern-cli` remains a small diagnostic executable.

Add another crate only when a boundary has distinct dependencies, ownership, or release needs.
Avoid a crate per feature and avoid a shared `utils` crate; keep helpers with the concepts that own
them.

## Runtime model

The render loop creates widgets only for visible cover-grid rows. A persistent query worker
coalesces superseded searches, a bounded worker pool loads and decodes cover thumbnails, a metadata
worker serializes edits, and a dedicated import worker parses publications in parallel before
committing bounded transactions. The UI retains at most 256 cover textures and requests repainting
only when background work completes or interaction requires it.

SQLite runs in WAL mode for the persistent library. FTS5 triggers keep search data consistent with
imports and metadata edits. Imports never extract archive members to disk, reject unsafe archive
paths, and cap XML, cover entry, decoded-image, PDF file, thumbnail, and batch sizes. PDF first pages
are rendered on the CPU without a system PDF runtime.

## Operational principles

- Treat the user's ebook files and metadata as durable data: migrations must be explicit,
  transactional where possible, and recoverable.
- Keep indexing and import work observable. Add cancellation when the UI exposes a cancel action.
- Build complete user workflows first; profile representative libraries before optimizing or making
  large-library performance claims.
- Use structured diagnostics internally and translate them to actionable messages at application
  boundaries.
- Keep platform-specific code behind adapters and exercise the workspace on all supported operating
  systems in CI.

Architecture decisions that constrain future work are recorded in `docs/adr/`.
