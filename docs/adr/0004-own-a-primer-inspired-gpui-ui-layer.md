# ADR 0004: Own a Primer-inspired GPUI UI layer

- Status: Accepted
- Date: 2026-08-25

## Context

Lectern is moving toward GPUI for its native interface and wants the visual language and interaction
contracts of GitHub's Primer design system. Primer has no GPUI implementation, and GPUI remains
pre-1.0 with regular breaking changes. A complete, general-purpose Primer port would commit Lectern
to a large compatibility surface before the application has proved which components it needs.

The upstream systems also have different responsibilities. Primer's product documentation,
primitives, and reference implementations describe visual and behavioral contracts. GPUI and
`gpui-base` provide native rendering, focus, input, accessibility, and interaction machinery. React,
CSS Modules, DOM events, HTML attributes, portals, refs, and hooks are not useful native API
contracts.

## Decision

Create an internal workspace crate named `lectern-ui`. It is Lectern's design-system adapter, not a
crate named `primer-gpui` and not a promise to implement or publish all of Primer.

Use three explicit layers:

```text
Lectern product UI       BookCard, LibrarySidebar, BookGrid, ReaderToolbar
          │
          ▼
lectern-ui               Primer-styled Text, Button, ActionList, Dialog, ...
          │
          ▼
gpui-base + GPUI         native behavior, state, focus, accessibility, rendering
```

Port a component only when a Lectern journey needs it. A port preserves the relevant Primer visual,
state, interaction, keyboard, and accessibility contract through a typed Rust API. It does not
translate the web implementation or expose web escape hatches such as `className`, arbitrary HTML
element selection, or ARIA strings.

Import Primer Primitives through a deterministic `cargo xtask primer-sync` generator. Pin the exact
upstream package version and integrity in a checked-in source manifest, consume its resolved
`dist/docs/**/*.json` artifacts, and generate typed Rust values. Generated files are committed and
carry provenance; production code does not parse JSON or resolve design tokens at runtime. The sync
fails on an unsupported schema, value, unit, missing required token, or generated-name collision.
Components must not contain copied color literals or parallel hand-maintained token values.

Represent each complete theme as immutable data. Begin with `PrimerTheme::light()` and
`PrimerTheme::dark()`, install the current theme in GPUI typed global state, and keep component code
independent of the selected color mode. The representation must accept additional upstream token
sets, including high-contrast and color-vision variants, without changing component implementations.

Vendor only the required Tabler outline SVGs through a separately pinned and checksummed source.
Generate a closed Rust `TablerIcon` enum that maps variants to static asset paths. Preserve the
upstream MIT license and provenance. Brand icons are excluded unless their product and trademark use
has been explicitly reviewed.

Pin `gpui`, `gpui_platform`, and `gpui-base` deliberately. `gpui` and `gpui_platform` must resolve
from the same source revision, and `gpui-base` must be selected from a revision whose own GPUI
dependency resolves to that identical source. A lockfile coincidence or matching version string is
not enough when Cargo sees different package sources as different types.

Maintain a small component-gallery executable from the first component onward. It renders every
supported theme, size, variant, content shape, and semantic state, and accompanies GPUI tests for
activation, actions, focus, keyboard navigation, controlled state, and accessibility properties.

The implementation workflow and definition of done are in the
[Primer-to-GPUI porting guide](../porting-primer-to-gpui.md).

## Consequences

- Lectern owns a small, coherent native UI API without taking responsibility for all of Primer.
- Upstream token and icon updates are deliberate, reviewable generated diffs rather than scattered
  manual edits.
- Native behavior stays in the lowest suitable GPUI layer, while Lectern owns its presentation.
- Product components remain free to express ebook workflows without being misrepresented as Primer
  primitives.
- Framework, primitive, and icon upgrades require explicit compatibility review and regeneration.
- Each runtime port is performance-sensitive and must add or run a representative release-mode
  workload under `docs/performance-policy.md`; the documentation decision itself changes no runtime
  behavior.
