# ADR 0002: Model logical books separately from file assets

- Status: Accepted
- Date: 2026-08-24

## Context

Lectern originally stored a file format and source path directly on each `books` row. That made a
database book synonymous with one file. A single title available as EPUB and PDF therefore became
two unrelated books, and file lifecycle concerns could not evolve independently from metadata.

The library roadmap needs stable file identities for managed and reference storage, hashing,
deduplication candidates, missing-file detection, relinking, rescanning, export, and Calibre's
multi-format records. Those operations belong to files, while title, authors, series, description,
cover, tags, identifiers, and ratings belong to a logical book.

## Decision

Model a logical book as book-level metadata with one or more child assets:

```text
Book
├── metadata
├── optional cached cover
└── 1..N BookAsset
    ├── stable asset ID
    ├── format
    ├── storage mode
    └── reversible path
```

The SQLite `books` table contains only logical metadata and timestamps. `book_assets` owns format,
storage mode, and path. Each asset has a stable integer identifier and references its book with
`ON DELETE CASCADE`.

One logical book has at most one asset per format. This gives export and replacement deterministic
semantics and matches Calibre's `UNIQUE(book, format)` model. A referenced path is globally unique,
while a managed path may be shared by multiple logical records so a future content-addressed store
can deduplicate physical bytes without merging metadata records.

Reference paths use a reversible tagged byte encoding: UTF-8 where possible, raw bytes for
non-Unicode Unix paths, and little-endian UTF-16 units for non-Unicode Windows paths. Managed paths
must be portable UTF-8 paths relative to the managed library root.

Aggregate import is explicit. An importer that knows several files represent one book supplies one
record with several assets. Ordinary file discovery continues to produce one record per file and
does not merge by title, author, filename, or hash. A future Calibre adapter will use Calibre's
stable book identity to create the aggregate.

The library summary projection does not load or aggregate asset details. Format filtering means
"has an asset in this format" and uses an indexed `EXISTS` predicate, preserving exactly one result
row per logical book. Complete assets are loaded only with the book detail record.

Schema versions 1 and 2 migrate directly to version 3 in one immediate transaction. Book IDs,
timestamps, covers, and full-text row IDs are preserved, and every legacy book receives one
referenced asset. The migration validates foreign keys, the one-or-more-assets invariant, and FTS
consistency before advancing `user_version`.

The table-rebuild sequence follows SQLite's documented migration procedure, including disabling
foreign-key enforcement before the transaction and running `foreign_key_check` before commit.
SQLite recommends indexing child keys and multi-column query predicates; both asset access orders
are covered. Calibre's own schema independently validates the one-format-per-book constraint.

- [SQLite table-rebuild procedure](https://www.sqlite.org/lang_altertable.html#making_other_kinds_of_table_schema_changes)
- [SQLite foreign-key indexes](https://www.sqlite.org/foreignkeys.html#fk_indexes)
- [SQLite query planner](https://www.sqlite.org/queryplanner.html)
- [Calibre metadata schema](https://github.com/kovidgoyal/calibre/blob/master/resources/metadata_sqlite.sql)

## Consequences

- A book can represent EPUB, PDF, and future formats without duplicating logical metadata or its
  cover.
- Stable asset IDs make relinking and file-state updates possible without replacing a book.
- Storage ownership is explicit per asset, allowing gradual or mixed managed/reference conversion.
- Hashes, scan observations, and external provenance can be added to assets without reshaping the
  book table. Exact-byte matches must not silently merge logical books.
- Calibre import can map one Calibre book and its format set to one aggregate record. Rich Calibre
  metadata remains separate product work.
- The database cannot enforce "at least one child" with an ordinary foreign key. Transactional
  write APIs reject empty aggregates, and migrations and integrity tooling must check the invariant.
- Callers that import independent files retain a single-publication compatibility adapter, while
  new trusted aggregate importers use the multi-asset boundary.
