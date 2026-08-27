# Action menu native contract

## Journey and workload

The first call sites are the book-detail language selector, contributor-role selector, Series
picker, and tag picker. The representative release workload is `ui-book-detail-regression-v1`;
series and tag suggestion queries also use the bounded organisation autocomplete path. Persistent
tag-color and series-number changes are covered by the organisation migration workload before
commit.

## Supported API

- `ActionMenu` composes an application-owned trigger and menu body with the pinned `gpui-base`
popover lifecycle. It owns anchored deferred painting, outside-click and Escape dismissal, focus
capture/restoration, controlled open state, width, a small trigger-to-surface gap, and a bounded
scrolling surface.
- `ActionListItem` provides a full-width option row with selected, disabled, optional color-dot, and
  activation states. The visible label and semantic selected state must identify the choice without
  relying on color.
- `TagChip` provides the compact selected-tag pill. It requires a visible name, accepts a
  theme-resolved dot color, and exposes a removal action only when the caller supplies one.
- `EntityChip` provides the same compact removable identity without implying a color. Series uses
  it because color has no series meaning.
- Applications own option collections, filtering, async data loading, selection rules, creation
  stages, and domain validation. Menu components do not copy domain data into internal maps.

## Presentation and accessibility

Menus use the default surface, muted outline, small inset, and a large outer radius. Item radius is
derived concentrically from that outer radius and inset. Rows use medium control height but compact
vertical rhythm. Hover and selected backgrounds are theme-owned and remain visually quieter than a
primary action.

The popover surface exposes non-modal dialog semantics and restores trigger focus after dismissal.
Options expose listbox-option semantics and selected state. Triggers retain explicit accessible
names, and tag/color dots are supplemental to visible text.

Multi-select tag choices keep the menu open, including unsaved tag drafts in the selected-first
rows. Completing the distinct create-name-and-color journey closes the menu automatically.
Series uses the same search/create vocabulary pattern as Tags but is single-select: choosing an
existing result or creating a normalized series replaces the current chip and closes the menu.

## Performance contract

Menus render static slices or bounded result sets. Language options are assembled and sorted once
per process; tag suggestions are capped at 50 and loaded off the UI thread with stale-response
suppression. Do not allocate a string-keyed style map or query storage from a render function.
