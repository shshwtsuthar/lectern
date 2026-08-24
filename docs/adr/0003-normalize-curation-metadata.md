# ADR 0003: Normalize curation metadata and retain hot book projections

- Status: Accepted
- Date: 2026-08-25

## Context

Lectern currently stores authors and series as display strings on each logical book. This is enough
to render and full-text search a cover grid, but it cannot answer identity-based questions such as
“show every book credited to this person,” keep a series consistently named, order books within a
series, or apply a tag to many books. Tags and saved searches do not yet have a persistence model.

Replacing the strings with joins in every grid query would solve the identity problem at the cost
of making the hottest read path aggregate several many-to-many relations. Treating names as unique
only by their entered spelling would instead preserve duplicate contributors, series, and tags that
differ only in case or whitespace. Guessing that similar names identify the same entity would risk
silently corrupting metadata.

The existing logical-book/file-asset split remains correct. Contributors, series membership, tags,
and saved searches describe a logical book or a library projection; none belongs to a publication
file asset.

## Decision

Make contributors, series, and tags first-class library entities with stable identifiers. Store
their relationships to logical books separately from file assets:

```text
Book
├── 0..N ordered ContributorCredit ──> Contributor
├── 0..1 SeriesMembership ───────────> Series
├── 0..N BookTag ────────────────────> Tag
├── derived browser/search projection
└── 1..N BookAsset

SavedSearch ──> canonical LibraryQuery using stable entity IDs for exact facets
```

A contributor credit owns a role and an order. The first slice supports `author`, `editor`,
`translator`, `illustrator`, and `other`; a person may hold more than one role on the same book but
may appear only once per role. Author-role credits, in their explicit order, produce the author
line shown on a cover card. A contributor owns a display name and an independently editable sort
name. Lectern does not automatically invert personal names.

A book has at most one series membership in this slice. A membership may have an optional exact,
non-negative decimal index with no more than six fractional digits. Duplicate indices are valid.
Series order is normalized series name, indexed entries before unindexed entries, numeric index,
normalized title, then stable book ID. Multiple simultaneous series memberships are a later model
change, not a hidden capability of this relation.

Tags are flat, library-wide labels. A book/tag pair is unique and assignment is idempotent. Tag
order is display-name order; relation insertion order has no product meaning.

Contributor names, series names, tag names, and saved-search names share one identity-key
algorithm: Unicode NFKC normalization, leading/trailing Unicode-whitespace removal, internal
whitespace collapse to one ASCII space, and Unicode case folding. The entered display form is
preserved separately. Punctuation, diacritics, initials, word order, and aliases are not removed or
rewritten. Consequently, `Science Fiction` and ` science   fiction ` identify the same tag, while
`Ursula Le Guin` and `Ursula K. Le Guin` remain distinct contributors. A rename that collides with
an existing key must become an explicit merge.

The normalized relations are authoritative, but bounded browser and search projections remain
denormalized at book level. At minimum the projection retains the ordered author display string,
author sort key, optional series display/index, cover/file-issue flags, and the text needed by FTS.
Every metadata, relationship, rename, merge, import, and migration transaction updates its affected
projection and FTS rows before commit. Grid-page queries must not aggregate contributors or tags,
and normalized joins used for exact filters must be indexed semi-joins that still return one row per
logical book. Complete contributor, series, tag, and asset records are loaded only for book detail.

Saved searches persist canonical query state rather than widget state or only a rendered query
label. Exact contributor, series, and tag facets refer to stable IDs; text-search clauses retain
their text semantics. Entity merge rewrites saved-search references in the same transaction.

The migration from flattened metadata is deliberately lossless and conservative:

- each distinct non-empty legacy `authors` value becomes one author credit and one contributor;
- legacy author text is never split on commas, semicolons, ampersands, or the word “and” because
  the database does not retain enough provenance to distinguish separators from a name;
- each distinct non-empty legacy `series` value becomes one series membership without an index;
- existing author and series display values remain unchanged in the derived book projection;
- existing books begin with no tags, and no saved searches are synthesized; and
- the migration performs no publication-file reads or metadata reparsing.

New EPUB imports preserve separate creator elements as separate ordered author credits. A PDF
Author field remains one credit because its internal delimiter semantics are unknown. Automatic
fuzzy merging, subject-to-tag import, and filesystem-based migration repair are outside this
decision.

## Consequences

- Contributors, series, and tags can be renamed or merged once and remain consistent across the
  library, filters, and saved searches.
- Exact facet filters use stable identity while fielded text search can still match partial names.
- The cover grid keeps its bounded summary cost and cannot duplicate a logical book because it has
  several contributors, tags, or assets.
- Normalized writes are more involved: each mutation must maintain relationship constraints,
  derived display/search state, and FTS atomically. Integrity diagnostics must verify or rebuild
  those projections.
- Existing multi-author strings may initially appear as one contributor. Correcting them is an
  explicit user edit rather than a migration guess.
- Contributors with the same normalized display name are one entity in this first model. Homonym
  disambiguation, aliases, external authority identifiers, and fuzzy duplicate suggestions require
  a future decision.
- Changing to multiple series memberships or hierarchical tags requires an explicit schema and
  product migration; neither is implied by this model.
