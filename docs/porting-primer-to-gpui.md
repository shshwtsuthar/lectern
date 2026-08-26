# Porting Primer to GPUI

## Purpose

Lectern will use Primer's design language through an internal Rust crate named `lectern-ui`. This is
an application design system for Lectern, not a complete port of Primer and not a general-purpose
`primer-gpui` project.

The work proceeds one component at a time. A component is added only for a concrete Lectern user
journey, with its upstream contract, native behavior, accessibility, visual states, tests, and
performance evidence completed in the same slice. Do not build a catalogue in anticipation of
possible future use.

The architectural decision is recorded in
[ADR 0004](adr/0004-own-a-primer-inspired-gpui-ui-layer.md).

Lectern's product-specific brand color and nested-surface geometry rules are defined in the
[visual foundations](ui/visual-foundations.md). These rules supplement Primer and apply to every
Lectern UI implementation and review. Primer is a derived starting point, not Lectern's final visual
authority: product-specific foundations take precedence where the systems intentionally differ.

## The boundary

Primer, `lectern-ui`, and GPUI answer different questions:

| Layer | Owns | Does not own |
| --- | --- | --- |
| Primer sources | Derived design intent, tokens, variants, sizes, state appearance, usage guidance, accessibility expectations | Lectern's brand identity, Rust API, or native implementation |
| `lectern-ui` | Typed component APIs, Primer token mapping, Lectern brand tokens and visual differentiation, presentation, component composition, accessible product semantics | Low-level text editing, IME, focus machinery, platform event translation |
| `gpui-base` | Reusable control behavior, controlled state, keyboard and pointer activation, focus, accessibility roles, overlay and input infrastructure | Primer presentation or Lectern product concepts |
| GPUI | Rendering, layout, windows, assets, actions, typed global state, and platform integration | Primer compatibility |
| Lectern product UI | Ebook workflows such as the library grid, sidebar, book card, metadata editor, and reader toolbar | Pretending product-specific components belong to Primer |

`BookCard`, `LibrarySidebar`, `BookGrid`, and `ReaderToolbar` remain Lectern components composed from
`lectern-ui`. The likely initial system-component set is:

- `Text` and `Heading`;
- `Button` and `IconButton`;
- `TextInput` and `SearchInput`;
- `Tooltip`;
- `ActionList`, `ActionMenu`, and `NavList`;
- `Dialog` and `Overlay`;
- `Spinner`; and
- `Label`.

This is a candidate backlog, not a requirement to implement the entire list before product work.
Dependency order is allowed to differ. For example, a useful `IconButton` may require `Tooltip`
first, and `ActionMenu` should compose the existing `ActionList` and overlay behavior.

## Upstream source policy

### Pin source identity, not just a compatible version

The first `lectern-ui` bootstrap must add a checked-in source manifest, expected at
`tools/primer/primer-sources.toml`, containing:

- the exact `@primer/primitives` package version and package integrity;
- the exact Tabler Icons package version or Git commit and archive integrity;
- the Primer React version or Git commit used as the component reference;
- the GPUI Git URL and full commit SHA;
- the `gpui_platform` Git URL and the same full commit SHA; and
- the `gpui-base` Git URL and full commit SHA known to use that exact GPUI source.

Do not use `main`, `latest`, a semver range, or an unqualified Git branch. Keep the Cargo lockfile,
but do not treat it as the human-readable source manifest.

GPUI is pre-1.0 and documents that breaking changes are expected. `gpui-base` also warns that an
application must resolve the same GPUI source as the base crate. Matching crate names or version
numbers do not make types compatible if Cargo resolves two different package sources. Before every
GPUI-family upgrade, verify the graph with commands equivalent to:

```sh
cargo tree -i gpui
cargo tree -d | rg 'gpui|gpui_platform|gpui-base'
```

There must be one intended GPUI package identity. Upgrade GPUI, `gpui_platform`, and `gpui-base` as
one compatibility change, independently from a component port.

### Source precedence for a component

Research a component in this order:

1. Primer's Product UI overview, guidelines, and accessibility page define the user-facing
   contract.
2. The pinned Primer React public types, stories, and tests enumerate variants and edge cases.
3. The pinned React source explains ambiguous behavior, but its implementation structure is not an
   API to copy.
4. Resolved Primer Primitives values define presentation.
5. The pinned `gpui-base` and GPUI source define which native behavior and accessibility facilities
   are actually available.

Primer also has Rails implementations for some components. They can corroborate semantics, but
neither Rails nor React should dictate a native implementation detail.

At the beginning of a port, record the upstream URLs, retrieval date, pinned commits or versions,
and any deliberate contract differences in the component module documentation or a short design
note. Do not vendor entire HTML documentation pages into the repository. Preserve concise findings
and stable source identities instead.

## Primer Primitives synchronization

### Required tool interface

The bootstrap slice must add a Rust `xtask` whose stable interface is:

```sh
cargo xtask primer-sync
cargo xtask primer-sync --check
```

The first command fetches or reads the exact locked package, verifies its integrity, parses the
approved resolved JSON inputs, and writes deterministic Rust. `--check` generates into a temporary
directory and fails if the checked-in output differs. It must also support an explicit local package
archive or extracted directory so generation can be reproduced without a mutable global npm cache.

The package verified during preparation of this guide, `@primer/primitives` 11.10.0, publishes the
following useful inputs:

```text
dist/docs/base/size/size.json
dist/docs/base/typography/typography.json
dist/docs/functional/size/*.json
dist/docs/functional/spacing/space.json
dist/docs/functional/typography/typography.json
dist/docs/functional/themes/light.json
dist/docs/functional/themes/dark.json
```

That observation is not a floating dependency declaration. The implementation source manifest is
authoritative and must contain its own exact version and integrity.

The resolved JSON files are objects keyed by token name. Entries currently contain fields such as
`name`, `path`, `type`, `value`, `description`, `filePath`, and `original`. For example, component
tokens such as `button-default-bgColor-rest` resolve to a final color for each theme rather than to
another token reference. Dimensions may be expressed in `rem`; alpha colors may use eight-digit hex;
borders, shadows, and typography may be compound values.

### Fail closed

The sync tool must validate all assumptions and report the package version, source path, token name,
and offending value when it fails. At minimum it rejects:

- an unverified archive or unexpected package name/version;
- a JSON root that is not the expected object;
- disagreement between an object's key and its `name`;
- a missing required token;
- a duplicate token or Rust identifier collision;
- an unexpected token type or value shape;
- an unsupported color syntax, unit, border, shadow, or typography form;
- a non-finite or out-of-range numeric value; and
- a generated diff in `--check` mode.

Do not substitute a fallback literal for a missing or unparseable token. Schema drift is an upgrade
failure requiring review.

Unit interpretation is part of the generator contract. Preserve `rem` semantics with GPUI's typed
relative units, or an equivalent typed representation resolved once during theme construction. If a
GPUI API requires logical pixels, define and test the Primer root-font mapping used for that
conversion. Never erase a unit implicitly, and let GPUI/platform scaling handle physical display
scale. Do not round dimensions to integers. Add fixtures for transparent colors, negative shadow
spread, multiple shadows, unitless line heights, font weights, and every compound value used by a
component.

### Generate only the deliberate surface

Keep a checked-in allowlist that names every token `lectern-ui` consumes and its expected type. Read
the complete upstream files needed to resolve that list, but generate only typed values used by the
current component set. This keeps compile time, binary size, and review diffs bounded while ensuring
that every used value comes from Primer.

Expected generated structure:

```text
crates/lectern-ui/src/generated/
├── mod.rs
├── primitive_metadata.rs
├── light.rs
└── dark.rs
```

Every generated file starts with a do-not-edit notice, source version, integrity, generator version,
and input paths. Output ordering and formatting are deterministic. Generated source is committed so
normal application builds do not need npm, network access, JSON parsing, or code generation.

Production components must not contain copied Primer colors, dimensions, radii, or shadows. A value
that is genuinely Lectern-specific belongs in an explicitly named Lectern token layer with a reason;
it must not masquerade as an imported Primer token.

### Upgrade procedure

Upgrade primitives separately from feature work:

1. Change the exact version and integrity in the source manifest.
2. Run `cargo xtask primer-sync`.
3. Review added, removed, renamed, type-changed, and value-changed tokens.
4. Run `cargo xtask primer-sync --check`, formatter, focused tests, and the component gallery.
5. Run the required release-mode UI performance workload because generated runtime styles can alter
   layout and paint work.
6. Commit the manifest, generator changes if required, generated diff, gallery evidence, and license
   updates as one primitives-upgrade change.

Never edit generated output to make an upgrade smaller. Change the mapping or allowlist and
regenerate.

## Themes are immutable data

Start with `PrimerTheme::light()` and `PrimerTheme::dark()`. The runtime representation should be
roughly equivalent to:

```rust
pub struct PrimerTheme {
    pub id: PrimerThemeId,
    pub colors: PrimerColors,
    pub spacing: PrimerSpacing,
    pub typography: PrimerTypography,
    pub controls: PrimerControlTokens,
    pub components: PrimerComponentTokens,
}

pub struct CurrentPrimerTheme(pub Arc<PrimerTheme>);

impl gpui::Global for CurrentPrimerTheme {}
```

This example shows ownership, not a guaranteed API for the eventual GPUI pin. Construct the complete
theme once, install the current `Arc<PrimerTheme>` as typed GPUI global state, and read it during
rendering. Theme switching replaces the global value and requests the necessary window refresh; it
does not rebuild token maps or parse files.

Components select variant, size, and semantic state from theme data. They do not branch on
`if dark`, compare theme names, or carry light/dark literals. Adding high-contrast, dimmed,
colorblind, tritanopia, or future upstream variants means generating another complete compatible
token set and registering it, not rewriting component render methods.

Prefer fixed typed structs and static slices to string-keyed maps in the render path. Style lookup
must be bounded and allocation-free. If a component needs derived presentation, calculate it once
when constructing the theme unless it truly depends on runtime geometry.

## Tabler Icons

Treat Tabler Icons as an independent upstream source and upgrade. Vendor only outline icons used by
committed components and product call sites. Tabler's SVGs use a 24 px view box; component sizing is
a rendering concern rather than a reason to vendor duplicated source variants.

Expected layout:

```text
crates/lectern-ui/assets/tabler/
├── search.svg
└── upload.svg
crates/lectern-ui/src/generated/tabler_icons.rs
third_party/tabler-icons/LICENSE
third_party/tabler-icons/PROVENANCE.md
```

Generate a closed enum and static mapping:

```rust
pub enum TablerIcon {
    Search,
    Upload,
}

impl TablerIcon {
    pub const fn path(self) -> &'static str {
        // Generated exhaustive mapping.
    }
}
```

The concrete rendering wrapper uses GPUI's SVG asset support. It should avoid allocating or
formatting paths during render. Icons inherit the component's semantic foreground color unless the
Primer contract gives the icon its own token.

An icon is decorative by default and must not invent an accessible name. The surrounding control
owns its label. `IconButton`, for example, requires an explicit accessible label even when a tooltip
shows the same text.

Tabler Icons are MIT-licensed. Preserve the license and provenance whenever SVGs are distributed.
Exclude brand icons by default; adding one requires an explicit product and trademark-use review,
not merely an enum variant.

## Crate shape

The first implementation may adjust module names, but it should preserve these ownership seams:

```text
crates/lectern-ui/
├── Cargo.toml
├── assets/tabler/
├── examples/component_gallery.rs
├── src/
│   ├── components/
│   │   ├── button.rs
│   │   └── ...
│   ├── generated/
│   ├── icon.rs
│   ├── lib.rs
│   ├── theme.rs
│   └── tokens.rs
└── tests/
```

Expose semantic components and their typed variants from `lectern-ui`. Keep generated modules and
raw token names private unless a strong use case requires otherwise. Product code should ask for
`ButtonVariant::Primary`, not `button-primary-bgColor-rest`.

Use `RenderOnce` for components composed from existing GPUI elements and base controls. Use a GPUI
entity/view only when the component owns retained mutable state. Text input state, selection, IME,
focus, menu selection, or overlay lifecycle must not be recreated on every render.

## What it means to port one component

### 1. Name the Lectern journey and workload

Before writing runtime code, identify the concrete call site and representative workload. Examples
include importing books from an empty library, searching a 50,000-book library, navigating the
library sidebar, or confirming a file replacement.

Classify the change under `docs/performance-policy.md`. A new or changed runtime component, theme,
asset path, layout, focus behavior, or render implementation is performance-sensitive. If no
deterministic release-mode workload covers the journey, add the scenario, correctness checks, raw
sample retention, p95 or corresponding tail metric, memory measures where relevant, and an explicit
budget before the component implementation commit.

### 2. Write the native contract first

Summarize only the relevant Primer contract:

- purpose and appropriate use;
- typed variants and sizes needed by Lectern;
- content slots and their layout rules;
- controlled and transient states;
- pointer, keyboard, focus, and dismissal behavior;
- accessibility role, name, description, values, and actions;
- required tokens and Tabler Icons; and
- deliberate omissions or native deviations.

Do not mechanically reproduce every React prop. A smaller API is correct when Lectern does not need
the omitted behavior.

### 3. Design a semantic Rust API

Prefer enums, required constructor arguments, builders, and callbacks that express intent. Keep
stable `ElementId` values at application call sites so GPUI can preserve element state and focus.

An illustrative Button call site is:

```rust
Button::new("import-books")
    .label("Import")
    .variant(ButtonVariant::Primary)
    .size(ButtonSize::Medium)
    .leading_icon(TablerIcon::Upload)
    .on_click(|_, window, cx| {
        // Dispatch a Lectern action.
    })
```

The actual callback and element APIs must follow the selected GPUI and `gpui-base` revisions.

Do not port or expose these web implementation concepts:

```text
React.ReactNode       className             CSS Modules
as="button"           forwardRef            useState/useEffect
DOM event types       HTML ARIA strings     React portals
```

Translate their purpose. Named content positions become typed optional fields or child builders.
Application state becomes GPUI entities or controlled values. Portals become GPUI overlay/deferred
paint infrastructure. ARIA requirements become GPUI/AccessKit roles and properties.

### 4. Delegate native behavior

Find the lowest suitable `gpui-base` control and wrap it. The foundation owns such behavior as:

- pointer and keyboard activation converging on one semantic callback;
- disabled and controlled state;
- focus handles, tab order, and focus traps;
- input editing, selection, clipboard, IME, undo, and caret behavior;
- menu navigation and selection where available; and
- accessibility roles and actions.

`lectern-ui` supplies Primer structure and presentation. It must not fork a text editor, synthesize
button behavior from raw mouse handlers, or implement an overlay focus trap when the pinned
foundation already provides one.

If `gpui-base` lacks a required behavior, first decide whether the behavior is reusable foundation
work or Lectern-specific composition. Keep generic behavior below presentation. Record and test any
temporary adapter; do not hide a semantic gap with visuals.

### 5. Map style from typed theme data

A component can resolve a compact style value and then apply GPUI pseudo-state refinements:

```rust
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: SharedString,
    variant: ButtonVariant,
    size: ButtonSize,
    // icon, handler, disabled, inactive, loading, ...
}

impl RenderOnce for Button {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<CurrentPrimerTheme>().0;
        let style = theme.components.button.resolve(self.variant, self.size);

        gpui_base::Button::new(self.id)
            // Apply dimensions and rest styles from `style`.
            // Apply hover, active, focus-visible, and disabled refinements once.
            .child(self.label)
    }
}
```

This is intentionally illustrative. Follow the pinned GPUI pseudo-state ordering and ensure each
pseudo-state has one owner; repeated hover/active/focus refinements can conflict in GPUI. Keep style
resolution allocation-free and do not clone a complete theme into each element.

### 6. Translate accessibility semantically

Map the contract to native accessibility rather than emulating HTML:

| Primer/web requirement | Native requirement |
| --- | --- |
| semantic element or `role` | GPUI/AccessKit role supplied by the base control |
| `aria-label` | accessible name/label property; make it a required Rust value when omission is invalid |
| `aria-describedby` | accessible description or relationship to the visible caption/error |
| `aria-disabled`/`disabled` | correct disabled or inactive state plus activation and focus behavior |
| `aria-selected`, `aria-checked`, `aria-expanded` | native selected, checked/toggled, and expanded properties |
| DOM keyboard handler | GPUI action/key binding routed through the focused semantic control |
| portal focus management | native overlay focus trap, initial focus, dismissal, and focus restoration |

Visible color or an icon is never the only state signal. Keyboard-visible focus is mandatory.
Leading/trailing decorative icons do not replace a text alternative. Where the pinned GPUI stack
cannot yet express a Primer accessibility requirement, record the gap explicitly and do not call the
component complete.

### 7. Add gallery coverage before the next component

The gallery is the native equivalent of Storybook. Each component page shows:

- light and dark themes side by side or through a deterministic switch;
- every supported variant and size;
- short, long, empty, and localized content shapes that the API allows;
- rest, hover, active, focus-visible, disabled, inactive, selected, open, validation, and loading
  states as applicable;
- leading/trailing visuals and missing optional slots;
- narrow and ordinary window widths; and
- a live interactive specimen in addition to any static state matrix.

Exercise hover, active, and focus through real interaction in the live specimen. A static state
matrix may use an internal style resolver, but must not add a public production escape hatch that
allows callers to force pseudo-states.

Do not begin a large batch of components without landing the gallery for the first one. The gallery
must remain runnable from a documented command such as:

```sh
cargo run --release -p lectern-ui --example component_gallery
```

### 8. Test behavior and presentation

Tests live beside the component and cover:

- exhaustive variant/size/state-to-token resolution;
- stable element identity where focus or keyed state depends on it;
- pointer activation on release and Enter/Space activation where appropriate;
- disabled, inactive, loading, selected, and controlled-state behavior;
- focus entry, focus-visible appearance, traversal order, trapping, dismissal, and restoration;
- accessibility role, label, description, value/state, and actions exposed by the pinned stack;
- both initial themes and any theme switch;
- generated token and Tabler Icon mapping completeness; and
- the component-specific contract and edge cases.

Use GPUI's test support for actions, focus, input, and windows. Keep a manual visual review step until
the selected GPUI stack provides a deterministic screenshot or accessibility-tree assertion for the
needed property; a test that merely compiles is not behavioral evidence.

### 9. Measure and commit the slice

Run the fastest applicable optimized workload before every runtime commit. Retain raw samples,
validate that the scenario reached the expected UI state, report p95 or the appropriate tail metric,
compare with the base revision on the same machine, and satisfy both absolute and relative budgets.

Useful component-level scenarios include action-to-next-painted-frame, menu/dialog open-to-painted,
text-input keystroke-to-painted, dense ActionList navigation, gallery render/layout/paint frame time,
allocation volume, and steady/peak memory. The application journey remains the final authority; a
fast isolated button does not prove that the import toolbar or book grid stayed fast.

Generated style changes, a new SVG path, a font change, dependency upgrades, gallery measurement
code, and UI tests that alter production features are not documentation-only changes. Follow the
full performance policy for them.

## Component-specific minimum contracts

### Button

Primer currently describes `default`, `primary`, `danger`, `invisible`, and `link` variants;
`small`, `medium`, and `large` sizes; leading and trailing visuals; disabled, inactive, and loading
states; and keyboard/accessibility behavior.

Port only the subset needed by the first Lectern call site, but design the enums so an intentionally
deferred variant can be added without stringly typed flags. A complete supported Button state means:

- the visible label is the accessible name;
- pointer, Enter, and Space activation reach the same action exactly once;
- disabled blocks activation and follows the native focus contract;
- inactive remains discoverable and can explain why the action is unavailable;
- loading prevents duplicate activation, preserves focus, keeps layout stable, and exposes an
  appropriate status announcement when supported;
- focus-visible is distinguishable from hover and ordinary pointer focus; and
- leading/trailing visuals remain decorative unless they carry independent visible information.

Do not implement link behavior by painting a button blue. If Lectern needs the `link` visual variant,
pair it with an explicit native navigation/open strategy and the correct accessibility role.

### IconButton and Tooltip

Require the icon and accessible label at construction. Show a tooltip with the concise visible label
on both hover and keyboard focus. Tooltip dismissal must include `Escape`, focus departure, and
pointer departure according to the adopted contract. Essential instructions must remain outside a
tooltip.

The icon remains decorative. A tooltip description may supplement the accessible label but does not
replace it. Size and visual variants follow the supported Button surface.

### TextInput and SearchInput

Wrap retained GPUI/`gpui-base` input state. Do not implement text storage, selection, cursor movement,
clipboard, IME, password masking, hit testing, or undo in `lectern-ui`.

The presentation layer owns small/medium/large sizing, background and border states, keyboard focus,
placeholder and value typography, leading/trailing visuals, loading, disabled state, validation,
caption/error layout, and action slots. A field must have a visible label unless a reviewed use case
provides an explicit accessible label. Captions and validation messages must be programmatically
associated with the input, and invalid state must not be color-only.

`SearchInput` is a specialization composed from `TextInput`: search visual, search-specific accessible
purpose, and an optional keyboard-accessible clear action. It does not fork the input engine.

### ActionList, ActionMenu, and NavList

`ActionList` owns the consistent item layout: label, description, leading visual, trailing visual or
action, groups/dividers, selection, active item, disabled/inactive explanation, danger treatment,
and loading placement. Its role and behavior depend on context; plain lists, menus, and listboxes do
not share identical keyboard or inactive-item rules.

`ActionMenu` composes an activating control, overlay, and ActionList with menu semantics. Opening
moves focus into the menu, arrow keys navigate, activation selects once, `Escape` dismisses, leaving
the menu dismisses where the adopted contract requires it, and focus returns to the trigger.

`NavList` contains navigation links/actions, has an accessible region label, exposes the current item
without relying on color alone, and preserves logical traversal order. Do not nest an unrelated
clickable control inside a navigation item when that creates conflicting semantics; use an explicit
trailing-action pattern if the native contract supports it.

### Dialog and Overlay

Use the foundation's overlay geometry, paint order, input occlusion, and focus-trap infrastructure.
A modal Dialog needs an accessible role and visible title, initial focus, focus containment, a
keyboard-accessible close path, `Escape` behavior, backdrop behavior chosen by the contract, blocked
interaction with underlying content, and focus restoration to its trigger.

Dialog content and actions remain composed Lectern UI. Do not model a React portal or maintain two
coordinate systems for hit testing and painting.

### Text, Heading, Spinner, and Label

Text and Heading map semantic typography roles to generated Primer type tokens. A visual heading
style and its accessibility/document hierarchy are separate inputs when necessary; do not choose
the semantic level solely from font size.

Spinner owns bounded animation and an accessible loading meaning supplied by its parent context. It
respects the platform's reduced-motion setting as supported by the pinned stack and does not trigger
unbounded application work.

Label maps Primer's label variants and sizes to generated tokens. Status meaning must be available in
text, not color alone.

## Definition of done for one port

A component is complete only when all applicable items are true:

- a concrete Lectern user journey needs it;
- its representative workload and performance classification are recorded;
- upstream sources and exact versions/commits are recorded;
- the supported native contract and deliberate omissions are documented;
- its Rust API is typed, semantic, and free of web implementation escape hatches;
- native interaction behavior is delegated to the lowest suitable GPUI layer;
- every presentation value comes from generated Primer or explicitly named Lectern tokens;
- light and dark work without component theme-name conditionals;
- required Tabler Icons are pinned, generated, licensed, and mapped statically;
- keyboard, focus, controlled state, and accessibility behavior are implemented and tested;
- the gallery covers every supported size, variant, slot, content shape, and state;
- focused tests, formatting, linting, and `primer-sync --check` pass;
- the release-mode workload retains correct raw samples and passes absolute and relative budgets;
  and
- the change lands as a small working commit without unrelated framework, primitive, or product
  work.

If any required behavior cannot be expressed by the pinned GPUI stack, the port is incomplete. Record
the exact blocker and resolve or explicitly rescope it before product code relies on the component.

## Reference sources

These links are discovery aids. The checked-in source manifest and per-component research record pin
the implementation inputs.

- [Primer Product UI](https://primer.style/product/)
- [Primer Primitives](https://primer.style/product/primitives/)
- [Primer token naming](https://primer.style/product/primitives/token-names/)
- [Primer color usage](https://primer.style/product/getting-started/foundations/color-usage/)
- [Primer Button](https://primer.style/product/components/button/)
- [Primer Button accessibility](https://primer.style/product/components/button/accessibility/)
- [Primer IconButton](https://primer.style/product/components/icon-button/)
- [Primer Text](https://primer.style/product/components/text/)
- [Primer Heading](https://primer.style/product/components/heading/)
- [Primer TextInput](https://primer.style/product/components/text-input/)
- [Primer TextInput accessibility](https://primer.style/product/components/text-input/accessibility/)
- [Primer ActionList](https://primer.style/product/components/action-list/)
- [Primer ActionMenu](https://primer.style/product/components/action-menu/)
- [Primer ActionMenu accessibility](https://primer.style/product/components/action-menu/accessibility/)
- [Primer NavList](https://primer.style/product/components/nav-list/)
- [Primer Overlay](https://primer.style/product/components/overlay/)
- [Primer Dialog](https://primer.style/product/components/dialog/)
- [Primer Dialog accessibility](https://primer.style/product/components/dialog/accessibility/)
- [Primer Tooltip](https://primer.style/product/components/tooltip/)
- [Primer Tooltip accessibility](https://primer.style/product/components/tooltip/accessibility/)
- [Primer NavList accessibility](https://primer.style/product/components/nav-list/accessibility/)
- [Primer Spinner](https://primer.style/product/components/spinner/)
- [Primer Label](https://primer.style/product/components/label/)
- [Primer search pattern](https://primer.style/product/scenario-patterns/search/)
- [Tabler Icons repository and license](https://github.com/tabler/tabler-icons)
- [GPUI crate documentation](https://docs.rs/gpui/)
- [GPUI typed global state](https://docs.rs/gpui/latest/gpui/trait.Global.html)
- [GPUI `RenderOnce`](https://docs.rs/gpui/latest/gpui/trait.RenderOnce.html)
- [GPUI examples](https://github.com/zed-industries/zed/tree/main/crates/gpui/examples)
- [`gpui-base` design and compatibility guidance](https://github.com/longbridge/gpui-component/tree/main/crates/base)
