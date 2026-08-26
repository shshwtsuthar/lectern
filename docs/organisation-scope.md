# Organisation and curation workflow scope

## Outcome

Lectern must progress from a fast cover browser into a library manager without weakening the
bounded browsing path that makes it fast. This tranche delivers one complete curation loop:

1. correct one book's contributors, series membership, series index, and tags;
2. select an explicit range or every book in the current projection;
3. add or remove tags from that selection in one atomic operation;
4. retrieve the result with exact facets or fielded search; and
5. save that complete projection for reuse.

The tranche is complete only when the same normalized contributor, series, or tag has one stable
library identity, bulk changes remain safe at representative scale, and the resulting organization
is immediately visible in the grid and search index. File assets, their paths, and their bytes are
not changed by any workflow in this document.

The durable model follows
[ADR 0003](adr/0003-normalize-curation-metadata.md). Normalized relations are authoritative while
compact derived book projections keep the virtualized grid and FTS path free from per-row
aggregation.

## Exact feature set

| Capability | Included in this tranche | Deliberate limit |
| --- | --- | --- |
| Contributors | Stable people, ordered credits, roles, sort names, rename, and merge | No aliases, authority IDs, or fuzzy matching |
| Series | One normalized series per book, optional numeric index, browse/filter, rename, merge, and series sort | One series membership per book |
| Tags | Flat normalized tags, single-book editing, rename, merge, delete, include/exclude filtering | No hierarchy, colors, or automatic rules |
| Selection | Toggle, ordered range, clear, and all-current-results selection | No persistent selection across query or library changes |
| Bulk actions | Add and remove several tags atomically; remove books from the library with confirmation and an exact affected count | No bulk contributor, series, title, file-asset, or export operations |
| Search | Safe fielded prefix/phrase search plus exact facet chips | Conjunctive grammar only; no raw FTS, OR, regex, or parentheses |
| Saved searches | Create, apply, update, rename, and delete a named query/filter/sort state | No smart collections or scheduled actions |
| Vocabulary management | Bounded contributor, series, and tag lists with usage counts and explicit merge | No automatic cleanup or duplicate suggestions |

This is one product tranche even though it should land as small implementation commits. Omitting
selection, bulk tag mutation, retrieval, or saved searches would leave an isolated data model rather
than a complete manager workflow.

## Domain and persistence contracts

### Stable identities

Add stable `ContributorId`, `SeriesId`, `TagId`, and `SavedSearchId` domain types. Relationships are
owned by a logical book and never by `BookAsset`:

- a book has zero or more ordered contributor credits;
- a credit contains a contributor ID, one role, and a zero-based position within that role;
- the supported roles are **Author**, **Editor**, **Translator**, **Illustrator**, and **Other**;
- the same contributor may have several roles on one book but only one credit per role;
- a book has zero or one series membership and an optional series index;
- a book/tag pair is unique; and
- unused entities remain available until deliberately removed or merged.

Names use the identity-key algorithm in ADR 0003. Creating an entity with an existing key returns
the existing entity instead of creating a case- or whitespace-only duplicate. A colliding rename is
rejected with an action to merge into the existing entity. Do not fuzzy-match, strip punctuation or
diacritics, invert a personal name, or split a name on punctuation.

Input validation is shared across import, desktop, service, and storage boundaries:

| Value | Accepted form |
| --- | --- |
| Contributor display/sort name | 1–256 Unicode scalar values after whitespace normalization |
| Series name | 1–256 Unicode scalar values after whitespace normalization |
| Tag name | 1–64 Unicode scalar values after whitespace normalization |
| Saved-search name | 1–80 Unicode scalar values after whitespace normalization |
| Series index | Optional exact decimal from 0 through 999999.999999, at most six fractional digits |
| Search expression | At most 1,024 UTF-8 bytes and 32 clauses |

Control characters are rejected. A series index is a decimal domain value, not an unchecked
floating-point value; canonical display removes redundant trailing zeroes. Duplicate indices in one
series are allowed and fall back to title then book ID for deterministic ordering.

### Book projections

The complete editable `Book` includes contributor credits, optional series membership, tags, and
assets. A `BookSummary` remains compact: it carries the derived author line and optional series
name/index but not contributor records, tag records, or assets. The cover card shows:

- ordered Author-role names as its contributor line;
- `Series name · #index` when an index exists, otherwise the series name; and
- no tag chips, because tag cardinality must not expand grid layout or summary allocation.

If a book has contributors but no Author-role credit, its card uses the first ordered contributor
and a role label instead of presenting the book as anonymous. Author sort uses the first Author
credit's sort name, falling back to the first contributor and then an empty value; books without an
author sort after named authors. Add **Series** to the existing title, author, and recently-added
sorts. Series sort is series key, indexed before unindexed, index value, title key, then book ID;
books without a series sort last.

Derived author, series, sort, and FTS values are updated in the same transaction as their source
relations. Integrity checks must detect a stale projection, and repair may rebuild it solely from
the database. Grid queries may use those derived values and indexed `EXISTS`/semi-joins; they must
not group contributors or tags or return duplicate book rows.

The minimum access-order coverage is equivalent to:

- credits by `(book_id, role, position)` and books by `(contributor_id, role, book_id)`;
- membership by `book_id` and series order by `(series_id, index, book_id)`; and
- tags for one book by `(book_id, tag_id)` and books for one tag by `(tag_id, book_id)`.

Exact index names and table layout remain storage-adapter concerns, but query-plan tests must prove
that representative contributor, series, and tag filters use their covering access paths.

### Rename, merge, and delete

Vocabulary mutations are library-wide transactions:

- **Rename** changes one display name and identity key, then rebuilds only affected book
  projections and FTS rows.
- **Merge** keeps an explicitly selected target ID, redirects source relationships and saved-search
  facets, removes duplicate relationships, rebuilds affected projections, and deletes the source.
- If both contributors credit the same book in the same role, merge keeps the earlier position and
  compacts that role's positions. Credits in different roles remain distinct.
- Tag merge deduplicates book/tag pairs. Series merge retains every membership because this slice
  permits only one series per book.
- A contributor or series can be deleted only when no book or saved search references it.
- Deleting a used tag requires confirmation naming both its book count and saved-search count. The
  transaction removes those assignments and facets; the affected saved searches remain, with that
  filter removed.

No vocabulary operation modifies a book asset or publication file. Failures roll back relationships,
derived projections, FTS, and saved-search references together.

## Single-book curation

Replace the free-form **Authors** field in **Book details** with an ordered contributor editor. Each
row has an entity autocomplete, role, sort-name access, reorder controls, and remove action. Typing
a new exact name offers **Create contributor**; selecting an existing name reuses its ID. A user can
split a conservatively migrated combined author string by removing that credit and adding the
individual contributors.

Replace the free-form **Series** field with an entity autocomplete and adjacent **Book number**
field. Clearing the series also clears its index after confirmation when the index is non-empty.
Choosing another series retains the index in the form until save so the user can decide whether it
still applies. Invalid index input is shown inline and cannot be saved.

Add a type-ahead **Tags** chip editor. Entering a new normalized label creates it as part of the
book-save transaction; removing a chip removes only that book/tag relationship. The picker shows
exact-name matches before prefix matches and includes global usage counts, but never creates an
entity from a highlighted value without an explicit selection or Enter action.

Saving book details atomically updates ordinary metadata, credits, series membership, tags,
derived projections, and FTS while leaving assets unchanged. A failed save keeps every edit in the
open form. Existing save/reset keyboard behavior remains.

## Multi-selection

Preserve the ordinary one-book workflow while adding conventional grid selection:

- an unmodified card click clears any multi-selection and opens that book's details;
- `Ctrl`-click on Windows/Linux or `Cmd`-click on macOS toggles one book without opening it;
- `Shift`-click selects the inclusive range from the current anchor in the current result order,
  including intermediate results whose summary pages are not cached;
- once selection is active, visible cards expose checkboxes and an unmodified checkbox/card toggle;
- **Select all matching** and `Ctrl/Cmd-A` while the grid owns focus select the complete current
  valid projection, not only loaded or visible pages; and
- `Escape` or **Clear selection** returns to ordinary browsing.

Range resolution occurs off the render thread and loads only stable IDs or an equivalent compact
range descriptor—never covers or full `Book` values. All-matching selection retains the current
canonical query as a descriptor plus explicit exclusions; it must not eagerly allocate every book
summary. The bulk bar distinguishes **N selected** from **All N matching selected**.

Any search, facet, sort, saved-search, import, removal, or other completed library mutation clears
selection. This prevents a hidden or stale target set. Opening a bulk panel closes the single-book
editor; unsaved book edits must be saved, reset, or explicitly discarded first.

## Bulk tag workflow

The bulk-tag panel is intentionally restricted to tags. It displays the union of tags on the target
books with one of three observed states: **All**, **Some**, or **None**. For each tag the user may
queue **Add to all**, **Remove from all**, or leave it unchanged. Search can add tags absent from the
union, and **Create and add** creates a new tag only when the operation commits.

One **Apply tag changes** action submits the target descriptor plus disjoint add/remove tag sets.
The storage boundary resolves the target set and applies all changes in one transaction. Adding an
existing relationship and removing an absent relationship are successful no-ops. The result reports:

- books matched at transaction start;
- relationships added;
- relationships removed; and
- tags created.

An all-matching selection is invalidated if the library generation changes before apply; the user
must review the new count rather than silently targeting a different projection. A successful
operation refreshes the library, announces the exact counts, and clears selection. A failed
operation preserves the selection and queued changes for retry. Removing a tag used by the active
filter may make affected books disappear after the successful refresh; that is expected and must
not be reported as lost data.

Bulk tag work runs on the serialized library-write path. It must use set-based statements or bounded
batches inside one transaction, must not load each complete book, and must not issue one commit per
book. There is no bulk contributor edit, series edit, asset action, or filesystem operation in this
tranche.

## Bulk remove workflow

The selection bar exposes **Remove from library** for a non-empty, resolved selection. A destructive
confirmation names the exact selected count and states that publication files remain on disk. While
the operation is queued, the selection is locked and the bar presents a busy state.

The metadata worker sends the compact selection descriptor to one immediate transaction. The
storage boundary rejects stale query-backed selections, removes the selected logical books and
their database-owned assets, cached covers, organization relationships, and FTS entries with
set-based statements, and returns the exact removed count. It never opens, modifies, or deletes a
publication file. Success clears selection, releases cached cover state, refreshes the active
projection, and announces the exact result. Failure preserves the selection for review or retry.

The deterministic 50,000-book workload removes a 10,000-book query-backed selection. It retains 40
release-mode samples, verifies every cascade and source-file invariant, and gates durable removal
plus the first 128-book refresh at 3.5 seconds p95 with at most 32 MiB of additional process RSS.

## Filtering and structured search

### Exact facets

Add a **Filters** popover and an active-chip row without introducing an always-rendered library
sidebar. The compact toolbar continues to own format, file-health, and sort controls. Exact facets
support:

- zero or more required contributors, each optionally restricted to Author role;
- zero or one series;
- zero or more included tags, all of which the book must have;
- zero or more excluded tags, none of which the book may have;
- the existing format filter; and
- the existing last-observed file-health filter.

Contributor, series, and tag pickers return at most 50 prefix matches per request, put selected
items first, and show global usage counts. Counts are not recomputed relative to every active facet.
Every exact facet stores a stable ID. Tag include and exclude sets are disjoint; moving a tag to one
side removes it from the other.

All facet classes combine with AND. Required contributors each use an indexed existence check,
included tags use match-all semantics, excluded tags use match-none semantics, and the book's many
assets still collapse to one logical-book row. Active chips can be removed individually or cleared
together.

### Search grammar

The search box accepts safe, conjunctive field syntax in addition to bare terms:

| Form | Meaning |
| --- | --- |
| `dune` | Prefix term across title, contributor names, series, publisher, and tags |
| `"left hand"` | Ordered phrase across the same fields |
| `title:foundation` | Prefix term in title only |
| `author:le` | Prefix term in Author-role contributor names only |
| `contributor:"ursula le guin"` | Phrase in contributors of any role |
| `series:earthsea` | Prefix term in series name |
| `tag:"science fiction"` | Phrase in tag names |
| `publisher:ace` | Prefix term in publisher |
| `language:en` | Exact case-insensitive language value |
| `format:epub` | Has an EPUB asset; `pdf` is the other accepted value |
| `file:missing` | Has an asset last observed as missing |

`file:` accepts `available`, `missing`, `unreadable`, and `unchecked`; `unchecked` maps to the
stored unknown/not-checked state. Field names and enum values are ASCII case-insensitive. A quoted
value supports `\"` and `\\`; unmatched quotes, empty field values, unknown fields, invalid enum
values, excessive length, and more than 32 clauses are syntax errors.

All clauses and exact facets combine with AND. Repeating `format:` therefore finds a multi-format
book that has every requested format. Text terms retain the existing Unicode diacritic-insensitive
prefix behavior. Phrases require adjacent terms in order. User input is compiled to bound FTS and
relational parameters; raw FTS operators are never accepted.

The first grammar deliberately has no `OR`, unary negation, grouping, regex, arbitrary wildcard, or
field alias. Exact tag exclusion belongs to the filter popover. This keeps parsing, error messages,
saved semantics, and index planning deterministic. Search help lists the accepted fields and examples.

While a query is temporarily invalid, Lectern shows the error and its source span, does not dispatch
it, and leaves the last valid result projection visible. Saving a search is disabled until the
expression is valid.

## Saved searches

A saved search captures the complete canonical library projection:

- the validated structured-search expression;
- exact contributor, series, included-tag, and excluded-tag facet IDs;
- format and file-health filters; and
- sort order.

It does not capture selection, scroll position, an open editor, pending bulk changes, autocomplete
text, or dialog state. Saving an otherwise empty projection is valid, so a user can save a sort such
as **Recently added**.

The toolbar's **Saved searches** menu supports **Save current search**, apply, explicit **Update**,
rename, and delete. Names are unique under the shared identity key and are shown alphabetically.
Applying one replaces the whole query/filter/sort state, clears selection, resets result paging,
and marks the saved search active. Any subsequent query change marks it **Modified** without
silently overwriting it. Update is an explicit action.

Saved queries persist canonical domain values, not a serialized UI object. Their stored shape is
versioned. Exact entity facets use stable IDs, merge rewrites them, and tag deletion follows the
confirmed behavior above. Deleting a saved search never changes books or vocabulary and requires
confirmation unless the UI provides an immediate undo.

## Vocabulary management

Add one **Organise library** surface with **Contributors**, **Series**, **Tags**, and **Saved
searches** sections. Each vocabulary list is searched and paged off the UI thread, displays its
global usage count, and renders at most 100 rows at once. It provides the rename, merge, and delete
operations defined above; it is not a second book browser.

Merge always names source and target, reports affected book and saved-search counts before
confirmation, and states that no book files will change. Search and facet results refresh after a
successful vocabulary mutation. Failures retain the dialog state for retry.

## Import and migration

The schema upgrade is one immediate, validated transaction. It preserves book IDs, asset IDs,
covers, timestamps, file-health state, paths, source bytes, and the visible/searchable legacy author
and series strings. It creates no tags or saved searches and performs no filesystem reads. The
conservative conversion rules in ADR 0003 are mandatory, including treating a complete legacy
author string as one credit rather than guessing delimiters.

After the upgrade, new EPUB imports create one ordered Author credit per distinct `dc:creator`
element. The EPUB 2 `calibre:series_index` value and EPUB 3 `group-position` refinement populate the
series index when they satisfy the domain range; malformed index metadata is ignored without
failing an otherwise valid publication. A PDF Author value remains one Author credit. This tranche
does not turn EPUB subjects or PDF keywords into tags.

Re-import of a known reference path continues to update the existing logical book and must reconcile
normalized metadata without changing book or asset identity. User-curated contributors, series, or
tags must not be overwritten by a routine re-import; source-derived metadata may fill only fields
still marked as import-owned by the implementation contract. If ownership provenance is not added
in this tranche, preserve all existing curated relational values on known-path re-import.

Migration validation covers foreign keys, unique identity keys, contiguous contributor positions,
one-series-per-book, valid decimal indices, projection equivalence, FTS integrity, saved-search
references, and the existing one-or-more-assets invariant. Any failure rolls back the schema version
and all migrated data.

## Performance evidence

Every runtime part of this tranche is performance-sensitive under
[`docs/performance-policy.md`](performance-policy.md). Check in the versioned workloads and budgets
before their production paths. New scenarios have absolute budgets until a comparable base exists;
later changes also use the repository's paired 10%/material-delta regression rule.

### Normalized query workload

Add `organisation-query-regression-v1.json` over 50,000 books with deterministic distributions of
20,000 contributors, 2,500 series, 500 tags, eight tags per book, one to four contributor credits
per book, 70% series membership, existing mixed assets/covers, and 250 saved searches. Retain 10
warmups and 40 measured samples for:

- first 128 plus count for one contributor facet;
- first 128 plus count for one series facet and series sort;
- first 128 plus count for two included tags;
- first 128 plus count for included and excluded tags;
- a combined fielded-search, contributor, tag, format, and sort projection;
- a deep bounded page without recounting; and
- contributor, series, and tag autocomplete capped at 50 results.

Every first/deep page and autocomplete scenario has a 50 ms p95 product budget on the pinned runner.
Correctness checks reconcile exact IDs, result counts, ordering, absence of duplicate book rows, and
the query plan's intended covering indexes. All existing registered query and lifecycle suites also
run for schema, FTS, or query-plan changes.

### Selection and bulk-tag workload

Add `bulk-tags-regression-v1.json` over the same library. It must select all 10,000 books matching a
deterministic query without materializing summaries, add two tags and remove one in one durable
transaction, refresh the first affected page, then perform and verify the inverse operation. Retain
raw samples and check matched/added/removed counts, unchanged ordinary metadata and assets, exact
relationship sets, FTS/filter visibility, rollback on an injected failure, and no partial commits.

The durable mutation plus first-page refresh has a 500 ms p95 budget. Query/selection dispatch to a
painted busy state and completion to the refreshed painted grid each retain 40 compositor samples
and use the existing 50 ms p95 interaction budget. Peak additional process RSS during the 10,000-book
operation is capped at 32 MiB above the seeded idle phase; the operation must not allocate one full
`Book` per target.

### Migration workload

Add `organisation-migration-regression-v1.json` using independent copies of a version-five
50,000-book database. It verifies byte-equivalent visible author/series projections, stable book and
asset identities, FTS equivalence, empty initial tags/searches, and every schema invariant. Retain at
least 20 optimized samples; migration has a 5 second p95 wall-time budget and a 256 MiB peak-RSS
budget on the pinned runner. A migration error must leave the original database readable at its old
schema version.

Raw artifacts retain commands, samples, database/workload versions, host/toolchain data, query
plans, correctness reconciliation, and memory observations. A blocked applicable workload blocks
the runtime commit; do not defer measurement or relax these workloads in the feature commit.

## Implementation and commit sequence

Land the tranche in working, reviewable boundaries. The benchmark/budget commit is deliberately
separate from production changes:

1. Add normalized-query, bulk-tag, migration, and compositor benchmark scenarios with explicit
   budgets and correctness assertions.
2. Add shared identity normalization, stable IDs, decimal series-index parsing, typed query clauses,
   and pure domain/parser tests.
3. Add the transactional schema migration, normalized relations, indexes, derived projections,
   integrity checks, and migration benchmark evidence.
4. Add normalized storage/service reads and single-book writes with query-plan and rollback tests.
5. Preserve EPUB creator boundaries, parse supported series indices, adapt PDF metadata, and retain
   curated values on known-path re-import.
6. Replace the single-book contributor/series fields and add tag editing, keeping assets unchanged.
7. Add exact facet query planning, fielded search, series sort, bounded autocomplete, and their
   toolbar UI.
8. Add the bounded vocabulary manager with rename and merge before relying on normalized identity
   for long-term curation.
9. Add grid selection and the query-backed all-matching descriptor with compositor evidence.
10. Add atomic bulk tag mutation and its bulk panel with lifecycle and memory evidence.
11. Add versioned saved-search persistence and complete toolbar management.
12. Run the full quality/performance gates, update user-facing documentation and the changelog, and
    review every label against this contract.

Before every performance-sensitive commit, run the fastest new applicable release suite plus all
registered suites touched by its storage/query/rendering path. Preserve the raw artifact location
and result in validation notes. Never combine a workload reduction or budget relaxation with the
runtime change it would permit.

## Acceptance scenarios

The completed tranche must pass these product-level scenarios in addition to focused tests:

1. Migrate a populated library and observe identical book/asset IDs, source bytes, covers, author
   lines, series lines, search matches, and file-health results.
2. Correct a combined legacy author into two ordered credits, assign a series and decimal index,
   create two tags, save, reopen, and see the derived card/search data immediately.
3. Merge duplicate contributors, series, and tags and observe every affected book, exact facet, and
   saved search follow the chosen target without duplicate grid rows.
4. Select a range spanning unloaded pages, exclude one visible book, add two tags and remove one,
   and reconcile the reported counts and exact relationships after restart.
5. Select all 10,000 matching books, apply tags within the product budgets, and verify the UI remains
   responsive and memory bounded while the durable transaction runs.
6. Combine `author:`, a quoted `tag:` phrase, exact excluded tag, series facet, EPUB format, and
   series sort; verify stable paged results and a clear syntax error for an unsupported `OR`.
7. Save that projection, modify it without implicit overwrite, explicitly update it, restart
   Lectern, and recover the exact expression, stable facets, and sort.
8. Inject a failure into book save, vocabulary merge, and bulk tagging and observe complete rollback,
   retryable UI state, consistent FTS/projections, and unchanged publication files.

## Explicitly out of scope

- Hierarchical, colored, private, automatically assigned, or AI-generated tags.
- Manual shelves/collections, reading lists, reading status, progress, ratings, reviews, and dates
  other than the existing added/modified metadata.
- Bulk title, contributor, series, publisher, language, description, cover, asset, or export
  operations.
- Multiple series memberships, named series arcs, non-numeric volume labels, or automatic gap
  filling/renumbering.
- Contributor aliases, homonym disambiguators, external authority IDs, fuzzy matching, and automatic
  “Last, First” inversion.
- Raw FTS syntax, arbitrary boolean expressions, regex, content/full-book indexing, and search-term
  highlighting in covers.
- Automatic EPUB-subject/PDF-keyword tagging, Calibre import, filesystem folders as tags, and
  metadata reparsing during migration.
- Persisting selections, pending bulk changes, scroll position, or editor state across launches.
- Cloud sync, shared libraries, multi-user permissions, and cross-device saved searches.
- Any file move, rename, conversion, deletion, or change to the logical-book/file-asset ownership
  model.

These exclusions keep the first broad slice centered on a complete, fast curation workflow rather
than turning it into metadata authority resolution, reading tracking, or collection synchronization.
