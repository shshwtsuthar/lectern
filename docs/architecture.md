# Architecture

Lectern uses a Cargo workspace so the native application can compose independently testable domain,
storage, and import layers.

## Dependency direction

```text
lectern-desktop ──┐
                  ├──> lectern-service ──┬──> lectern-core
lectern-cli ──────┘                      ├──> lectern-import ──┬──> lectern-core
                                         │                    └──> lectern-storage
                                         └──> lectern-storage ───> lectern-core
```

`lectern-core` owns domain language, use cases, and the interfaces those use cases need. It must not
depend on a desktop framework, database driver, device SDK, network client, or platform API. Adapters
may depend on the core and implement its interfaces; the core must never depend on adapters.

`lectern-core` exposes one narrow `LibraryService` workflow contract rather than adapter-specific
transactions. The SQLite implementation lives in `lectern-service`, where product policy such as
preserving user-edited metadata on ordinary re-import is composed with parsing and persistence.
Desktop workers and the CLI invoke this workflow boundary; storage remains responsible for
mechanics and invariants, not frontend policy.

The CLI is a real diagnostic and automation surface, though Lectern remains a desktop-first
product. It can inspect statistics, run diagnostics, create a consistent backup, import
publications, and rescan referenced-file health against either the default library or an explicit
database path.

## Implemented boundaries

- `lectern-core` owns logical-book, asset, query, maintenance, and import language plus the narrow
  workflow interface, without infrastructure dependencies.
- `lectern-service` owns workflow orchestration and application policy while composing import and
  SQLite adapters.
- `lectern-storage` owns schema migration, transactional writes, FTS5 queries, and cover blobs.
- `lectern-import` owns EPUB/PDF discovery, bounded parsing and cover rendering, and batched
  ingestion.
- `lectern-desktop` owns native presentation, worker coordination, and platform dialogs.
- `lectern-cli` owns command parsing and human-readable automation/diagnostic output.

Add another crate only when a boundary has distinct dependencies, ownership, or release needs.
Avoid a crate per feature and avoid a shared `utils` crate; keep helpers with the concepts that own
them.

## Runtime model

The render loop creates widgets only for visible cover-grid rows. A persistent query worker
coalesces superseded searches and returns 128-summary windows; the first window also establishes the
matching count, while later windows skip the repeated count. The desktop retains at most six result
windows and at most one queued query request. A bounded worker pool loads and decodes cover
thumbnails, a metadata worker serializes edits, and dedicated import and asset-maintenance workers
perform filesystem and write-heavy work off the UI thread. The metadata worker also serializes
confirmed single- and bulk-book removal. Bulk removal resolves a compact selection and uses one
set-based transaction; both forms delete library records and cached data while leaving source files
untouched. Asset scans check referenced paths with
metadata and openability checks, then persist only changed health states in one transaction;
relinking validates publication structure without regenerating thumbnails. Explicit format
attachment validates the selected publication before atomically adding a referenced asset, leaving
book metadata, covers, and existing assets untouched. The UI retains at most 256 cover textures and
requests repainting only when background work completes or interaction requires it.

SQLite runs in WAL mode for the persistent library. FTS5 triggers keep search data consistent with
imports and metadata edits. Imports never extract archive members to disk, reject unsafe archive
paths, and cap XML, cover entry, decoded-image, PDF file, thumbnail, and batch sizes. PDF first pages
are rendered on the CPU without a system PDF runtime.

Books are metadata aggregates rather than files. A logical book owns one or more stable file assets;
format, storage ownership, and reversible paths live on those assets, while the cover remains
book-level. Library summaries deliberately omit asset payloads, and format filters drive an indexed
join from `(format, book_id)` so multi-format books still produce one grid row. Trusted import
adapters may supply several assets in one atomic record; ordinary file discovery never guesses
cross-format identity.

Persistent connections request WAL and use full synchronization. If SQLite cannot activate WAL for
a filesystem, full synchronization remains in effect for the returned rollback journal mode. Schema
creation and upgrades acquire an immediate transaction, validate relational and FTS integrity, and
advance the schema version only after validation succeeds.

Online backups use SQLite's backup API rather than copying the active database file. A backup is
written to a reserved sibling temporary path, checked for SQLite integrity and the expected book
count, synchronized, and then published without overwriting an existing destination. Diagnostics
report schema version, SQLite and foreign-key status, FTS consistency, missing book assets,
relationship validity, and referenced-file health. Managed-file/hash consistency remains an
explicit skipped check until managed storage exists.

## Operational principles

- Treat the user's ebook files and metadata as durable data: migrations must be explicit,
  transactional where possible, and recoverable.
- Keep indexing and import work observable. Add cancellation when the UI exposes a cancel action.
- Build complete user workflows first; profile representative libraries before optimizing or making
  large-library performance claims. Preserve the versioned 50,000-book full-result and paged-query
  budgets in `benchmarks/` when changing their storage paths, and use broader representative studies
  for other performance claims.
- Use structured diagnostics internally and translate them to actionable messages at application
  boundaries.
- Keep platform-specific code behind adapters and exercise the workspace on all supported operating
  systems in CI.

Architecture decisions that constrain future work are recorded in `docs/adr/`.
