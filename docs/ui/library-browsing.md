# Library browsing native contract

## Journey and information architecture

Lectern's default destination remains **All books**. A persistent, compact browse sidebar adds four
peer destinations: **Virtual Libraries**, **Genres**, **Contributors**, and **Series**. The sidebar
is application navigation, not a replacement for exact filters or the bounded vocabulary organiser.

Each metadata destination opens an index of library groups. Selecting one group drills into the
ordinary book browser with that stable group identity applied as a collection scope:

```text
All books
Virtual Libraries -> Favorites -> books
Genres             -> Science Fiction -> books
Contributors       -> Ursula K. Le Guin -> books
Series             -> Earthsea Cycle -> books
```

The scoped book browser preserves the existing book interactions: open details, explicit/range/all
matching selection, bulk tags, removal, device transfer, and asset actions. Query-backed selection
captures the collection scope so **Select all matching** never escapes the open group.

Tags remain exact include/exclude facets, compact book-detail chips, and a managed vocabulary. They
are not added to the browse sidebar in this slice because the committed journey names the four
metadata destinations above, and tag browsing would duplicate the existing multi-tag filter model.

## Navigation behavior

- **All books** is selected on launch and shows the complete library.
- Choosing a metadata destination clears a valid book selection, closes clean book details, resets
  paging, and loads the first bounded group page off the render thread.
- A dirty book editor or pending bulk edit blocks navigation and asks the user to save, reset, or
  finish the operation; navigation never discards edits silently.
- Selecting a group replaces the group index with its scoped books. The content bar shows the group
  type as a back action followed by the selected group name.
- Choosing the active sidebar destination while inside a group returns to that destination's group
  index. Choosing **All books** always returns to the complete library.
- Imports and completed mutations refresh the current route. If a group becomes empty, Lectern
  keeps the user in that group and presents its empty state instead of jumping elsewhere.
- Transient successes and errors remain in the fixed bottom bar. Navigation and loading feedback do
  not shift the content canvas.

The application top bar remains global chrome for appearance, device, creation, selection, and
import actions. The browse sidebar and content bar sit below it. The bottom bar retains the overall
or scoped book count at the leading edge and the latest notification at the trailing edge.

## Tile and table presentation

One display control in the content bar switches between **Tiles** and **Table**. Tiles are the
default on every launch; the chosen mode is retained while the process is open and applies to both
group indexes and book results.

Group tiles are restrained folder-like surfaces, not decorative dashboard cards. They show the
group name, exact book count, and only metadata that helps distinguish the group. Virtual-library
tiles may also show their selected built-in glyph and bounded description. Genre, contributor, and
series tiles do not invent cover art or color identities.

The group table uses compact rows with **Name** and **Books** columns, plus **Description** for
Virtual Libraries. Activating either a tile or row opens the same collection scope.

Book tiles preserve the current cover, centered title, contributor line, highlight, and selection
checkbox behavior. The book table uses one compact row per `BookSummary` with **Title**,
**Contributor**, **Series**, and **File status** columns. A row activation opens Book details; in
selection mode it follows the same toggle and modifier rules as a book tile. Table mode must not
load complete books, assets, tags, covers, or normalized relationship collections.

Both presentations use the same bounded page. Page controls expose the complete ordered result set
without eagerly allocating all summaries. Group pages contain at most 100 entries and book pages at
most 128 summaries. Changing page closes no clean route context and preserves the active display
mode; range selection uses global result offsets so it remains correct across pages.

## Layout and visual treatment

The sidebar is a continuous, quiet surface separated from content by `border.thin` and
`border.muted`. It uses the compact application-bar scale, one shared perimeter inset, and a
selected row whose background and non-color weight cue come from Lectern selection tokens. It must
not become a stack of rounded cards.

The content bar is a fixed compact bar with the documented large horizontal and small vertical
insets. Breadcrumb, display control, and paging controls remain visually subordinate to primary
actions. Content uses the muted bookshelf background; group tiles use the ordinary surface and a
quiet outline. Nested rounded geometry follows the concentric-radius rule in
[`visual-foundations.md`](visual-foundations.md).

At narrow widths the sidebar remains fixed and the book/detail surfaces take the remaining width;
labels truncate before controls overlap. The detail panel continues to appear on the trailing edge.
No group name, title, contributor, series, or description may expand application chrome or alter a
fixed bar's height.

## Empty, loading, and error states

- An actually empty canonical library retains the existing centered **Your library is empty**
  journey and does not show empty metadata indexes.
- An empty metadata index names the destination and explains where the relationship is assigned.
  Virtual Libraries additionally offers **Create Virtual Library**.
- An empty selected group says that the group contains no books and keeps its breadcrumb visible.
- Initial launch may use the full-window opening state. Later route and page loads keep the
  application shell stable and show a bounded content loading state.
- A failed route load leaves the chosen route visible, reports the actionable failure in the bottom
  bar, and offers a retry through the same navigation or page action.

## Accessibility and keyboard behavior

The browse sidebar has a visible **Browse** label and exposes one selected option without relying on
color. Group tiles and rows are native buttons with names that include the group name and book
count. The display control exposes the active Tiles/Table choice. Breadcrumb back, page, book row,
and selection controls participate in logical focus order and support Enter/Space activation.

Table headers are visible and maintain a consistent column order. Truncation does not change the
accessible label. Empty/loading text remains real visible text, and file problems retain a textual
status rather than a color-only marker.

## Domain and storage boundary

A collection location is modeled separately from a saved search or exact facet:

- **All books** has no scope;
- contributor and series scopes use stable normalized IDs;
- genre scope uses the closed `Genre` value; and
- virtual-library scope uses its stable ID.

The scope combines conjunctively with the ordinary `LibraryQuery` and is carried by query-backed
selection descriptors. Saved searches continue to store query/filter/sort state and do not silently
capture sidebar location. Group-index reads return typed stable identities and exact global book
counts; they are bounded and ordered by the existing normalized identity rules. Genre indexes
always include all 28 fixed choices, including zero-count genres, in catalog order.

Scoped book queries use indexed semi-joins and still return one row per logical book. They do not
aggregate normalized relationships in the hot summary path. Virtual-library, genre, contributor,
and series access orders must continue to use their checked covering indexes.

## Performance contract

This entire runtime path is performance-sensitive. The representative storage workload uses the
versioned 50,000-book organisation fixture, 20,000 contributors, 2,500 series, all 28 genres, 2,500
virtual libraries, and the production 128-summary page path. It retains 10 warmups and 40 measured
samples for:

- the first 100-entry Contributors, Series, Genres, and Virtual Libraries index pages with count;
- the first 128 books plus count inside one representative group of each type; and
- a deep scoped book page without recounting.

Every scenario validates exact identities, counts, order, lack of duplicate books, and the intended
covering query plan. Index-page and scoped-page latency each have a 50 ms p95 product budget on the
pinned runner, with the repository's paired relative-regression controls and retained raw samples.

The compositor workload launches the optimized GPUI application with the bounded 128-book page and
representative group entries. It retains 5 warmups and 40 measured fresh-process samples for group
navigation to painted index, group activation to painted scoped books, and Tiles-to-Table to painted
state. Each interaction has a 50 ms p95 product budget and an explicit peak-RSS budget. Correctness
markers reconcile the selected sidebar destination, breadcrumb, group counts, bounded book page,
table columns, and unchanged selection descriptor scope.
