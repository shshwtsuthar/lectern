# Text input native contract

## Journey and workload

The first call site is the right-side book-details editor: changing title, contributor names,
series book number, publisher, publication date, description, and the search queries inside the
Series and Tag menus for one selected book. Language, Series identity, and contributor roles use the
ActionMenu contract rather than a free-form metadata field. Its
representative release workload is `ui-book-detail-regression-v2`, which opens a complete book
fixture with normalized contributors, series membership, tags, and two assets. It retains raw
samples and checks open-to-painted-detail p95, first-paint latency, peak RSS, and fixture
correctness. `ui-bootstrap-regression-v1` also guards the component gallery and shared theme path.

## Supported API

- `TextInput::new(id, accessibility_label, state)` renders a medium, full-width single-line field.
- `TextArea::new(id, accessibility_label, state)` renders a full-width multi-line field;
  `height` controls only the visible frame while the retained editor owns scrolling.
- Every field requires a stable GPUI identity, a non-empty accessible name, and an application-owned
  `Entity<InputState>` or `Entity<TextareaState>`.
- Placeholder, initial value, disabled/read-only state, validation, and change subscriptions remain
  on the retained `gpui-base` state. This avoids a second source of truth in `lectern-ui`.
- Pointer focus, keyboard movement and selection, clipboard operations, undo/redo, and IME behavior
  are supplied by the pinned `gpui-base` editing engine.
- A pointer press outside a focused field clears focus during capture; a press on another field then
  focuses that target normally.

## Presentation and accessibility

The field frame follows Primer TextInput's medium control geometry: resolved control background,
foreground, placeholder, border, disabled colors, medium height and inline padding, medium radius,
and thin border. Focus uses the selected Lectern accent color and shared width. `TextArea` deliberately
shares the same frame and adds the medium spacing token as block padding.

Entered text, placeholder text, selection, and caret colors are installed into the retained editor
on render so switching the immutable Lectern light/dark theme also refreshes native text painting.
The root exposes the text-input role and the required accessible name. Product labels remain visible
outside the field, but accessibility does not depend on placeholder text.

## Deliberate omissions

Small and large sizes, leading/trailing visuals, loading, password reveal, validation badges,
character counts, and auto-growing text areas are outside this slice. A native browser-like resize
handle is also omitted because the fixed sidebar owns field width and the application owns
description height. These APIs require a committed product journey, additional token mapping,
gallery states, interaction/accessibility tests, and release performance evidence before addition.

## Pinned references

Contract research was performed on 2026-08-27 against Primer React
`b1117811cebfb9463f20fe76f77cdf13917ae6b2` (`TextInput`, `TextInputWrapper`, and `Textarea` source,
stories, and CSS) and `@primer/primitives` 11.10.0. Exact Primer, GPUI, `gpui_platform`, and
`gpui-base` source identities are recorded in `tools/primer/primer-sources.toml`; `cargo xtask
primer-sync --check` validates generated output and the resolved GPUI source revision.
