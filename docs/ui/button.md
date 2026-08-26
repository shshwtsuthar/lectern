# Button native contract

## Journey and workload

The first call site is the primary **Add books** action in an empty library. Its representative
release workload is `ui-bootstrap-regression-v1`: a fresh GPUI process paints the exact empty state,
then the production action transition changes the Button to the disabled **Adding books…** state.
The gate retains 40 measured samples after 5 warmups and checks initial-render and
click-to-painted-busy-state p95 plus peak RSS.

## Supported API

- `Button::new(id, label)` requires a stable GPUI identity and a text label. The same text supplies
  the accessible name.
- `ButtonVariant` supports `Default` and `Primary`.
- `ButtonSize` supports `Small`, `Medium`, and `Large`.
- A Button may have one decorative leading `TablerIcon`.
- `disabled` is application-controlled. A disabled Button has no activation action and does not
  participate in focus traversal.
- `on_click` handles pointer release and native Enter/Space activation through `gpui-base`.

The gallery covers both themes, every supported variant and size, the leading-icon slot, and the
disabled state. The empty-library view owns its asynchronous busy copy and disables the Button; the
component does not hide application state in an internal loading flag.

## Presentation and accessibility

All colors, geometry, typography, and focus-visible values come from the generated Primer token
allowlist. Components select immutable light or dark theme data without branching on a theme name.
The upload glyph is the pinned Tabler outline SVG and is decorative; the Button label owns the
accessible name.

The root exposes the Button role, label, and click action. Disabled controls remove their click
action. The pinned GPUI stateful-element API does not currently expose AccessKit's disabled flag, so
tests record the supported action surface without claiming that unavailable property.

GPUI has no separate outline primitive with `outline-offset`. The generated Primer focus color and
width refine the existing border on focus-visible, producing the intended inset ring without
changing outer control geometry; the generated negative offset remains in the immutable theme for a
future native outline primitive.

## Deliberate omissions

Danger, invisible, link, icon-only, trailing visual, counter, selected, and component-owned loading
APIs are not part of this first slice. They require a committed Lectern journey, their additional
generated tokens, gallery coverage, interaction/accessibility tests, and release performance
evidence before being added.

## Pinned references

Exact Primer Primitives, Primer React, GPUI, `gpui_platform`, `gpui-base`, and Tabler Icons source
identities are recorded in `tools/primer/primer-sources.toml`. `cargo xtask primer-sync --check`
validates generated outputs and the single resolved GPUI source revision.
