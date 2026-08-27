# Visual foundations

These foundations define Lectern's product-specific visual direction. `lectern-ui` is derived from
Primer, but Lectern is not a Primer replica and must remain visually distinct from GitHub. Primer
supplies useful component structure, interaction patterns, and primitive foundations; it does not
define Lectern's brand.

When Lectern intentionally differs from Primer, represent the difference with an explicitly named
Lectern token rather than disguising it as an imported Primer primitive. Do not inherit a GitHub
brand convention merely because the upstream Primer component uses it.

## Product temperament

Lectern should feel calm, compact, and precise: a fast personal-library tool rather than a spacious
marketing surface or a collection of dashboard cards. Let the books and their metadata carry the
visual interest. Application chrome should be restrained, controls should be no larger than their
task requires, and repeated actions should stay close to the content they affect.

The main top and bottom bars establish the application's density. On GNOME/Linux the application
requests the compositor's ordinary server-side title bar so window move, minimize, maximize, and
close behavior remains native; Lectern's own top bar stays application chrome beneath it. Side
panels, toolbars, menus, and
utility actions must use that same compact scale unless a larger target is needed for a primary
journey or accessibility. Prefer the small control size for local Save, Reset, Add, Remove, reveal,
and reorder actions. Use medium controls for ordinary text entry and prominent global actions.

Do not repeat context merely to fill space. If the selected book is already visibly highlighted,
the detail-panel title should say **Book details** without repeating the book name beside it. Keep a
book visibly highlighted while its details are loading or open so the relationship between grid and
panel remains clear. Selection must also have a non-color cue whenever it represents an actionable
multi-selection state.

## Dense side panels and section rhythm

A detail side panel is one continuous surface. Divide its major groups with flush, low-contrast
separators and shared horizontal insets; do not wrap every group in a rounded card or nested
container. Group tightly related ordinary metadata together, then give files, series, contributors,
tags, and library-level destructive actions one section each.

Within a section, use the smallest spacing token between a label and its control, the medium token
between related controls, and the large token between distinct field groups. Section headings and
the panel heading share the same leading inset. A fixed 48 px application or panel bar may use the
large horizontal inset and small vertical inset so its medium control remains centered; this is the
documented compact-bar exception to equal perimeter padding.

Use progressive disclosure for secondary choices. Contributor role choices, the language list, tag
search, and tag-color creation belong in anchored menus rather than expanding a virtualized section
or leaving every option permanently visible. Removing an advanced field from the common editor is
appropriate when its value can be preserved safely and managed in a dedicated vocabulary surface.

Anchored surfaces retain one small spacing token between trigger and menu so their outlines never
visually collide. Validation feedback stays in the section that owns the failing value; a
contributor error must not appear in Files merely because Files happens to be the next rendered
section.

## Brand color

Lectern's primary brand color is **Lectern Mauve** (`#9B6AA6`). It is a soft, luminous
mauve-lavender chosen to give the application a distinctive, literary identity without reading as a
conventional saturated purple. **Lectern Lavender** (`#D8C4E1`) is its supporting light tint for
restrained accents, highlights, and decorative treatments; it is not a substitute for the primary
color when the brand must be immediately recognizable.

Lectern Mauve replaces the usual GitHub green in brand-primary roles, including primary actions,
and remains the default accent. The Appearance dialog may replace it with Slate, Coral, Amber, Mint,
Azure, or Lilac through complete named accent token sets; the choice applies to primary actions,
focus, and selection and persists beside the local library. Do not use GitHub green as Lectern's
default primary-action color. Green may still be used when it
communicates an independent semantic meaning such as success, provided that meaning is not conveyed
by color alone.

Use named Lectern brand tokens when these colors enter production code. Do not scatter hexadecimal
literals through components or relabel the colors as Primer primitives. Interactive, hover, active,
disabled, focus, and theme-specific values must be defined deliberately in the theme layer rather
than calculated ad hoc during rendering.

Focus-visible outlines use the selected Lectern accent's named light/dark token; the default is
mauve in light mode and supporting lavender in dark mode. Do not expose Primer's default blue focus
accent through Lectern controls.

Brand color does not override semantics or accessibility. Do not use brand color alone to
communicate status, and verify the contrast of the actual foreground/background pair for its text
size and interaction state.

## Typography

Karla is Lectern's application typeface. Apply it at the application root so ordinary labels,
metadata, controls, status copy, dialogs, and headings inherit one consistent family. Preserve the
semantic size, weight, line-height, and color tokens for hierarchy rather than introducing
component-local font declarations.

Newsreader's bundled 14 pt optical-size Medium face is reserved for the **Lectern** wordmark in the
main top bar. Render that wordmark at Medium 500 through explicitly named typography theme tokens.
Do not use Newsreader as a general heading face; its literary character stays distinctive when the
rest of the interface remains Karla. The application embeds a wordmark-only subset of the face and
its OFL license so the wordmark never depends on a workstation font install or parses unused glyphs
at startup. Give the wordmark the large horizontal inset
used by other application bars, not the smaller vertical inset.

Library-card metadata is centered beneath its cover. Use the small spacing token between the cover
and title. Title and author use the shared compact metadata line-height with no additional margin,
so the perceived gap follows the Karla glyphs rather than its ordinary body-text line box. The
title uses a bold weight; the author uses the muted foreground and normal body weight. Both lines
remain bounded to the card width and truncate independently so long metadata cannot disturb the
grid.

## Bottom-bar notifications

Transient operation feedback belongs in the persistent bottom bar, not in the bookshelf canvas.
This includes completed actions such as books removed or tags applied, cancellation feedback,
recoverable errors, and similar future notifications. Keep the library count at the leading edge
and place the latest notification at the trailing edge, truncating it before it can displace the
count or change the bar's fixed height.

The bookshelf canvas is reserved for books, empty/loading states, and durable content guidance such
as pagination scope. Do not insert transient feedback above the grid, because doing so shifts book
positions and makes repeated operations visually unstable.

## Tags and supporting color

Tags use compact, rounded pills with their text label and a small named color dot. The dot is a
supporting recognition aid, never the only way to identify a tag. Selected pills sit immediately to
the left of the **+ Tag** action and wrap as one compact row.

The durable palette is **Slate**, **Coral**, **Amber**, **Mint**, **Azure**, and **Lilac**. Define
light- and dark-mode values as named tag-palette tokens in the Lectern theme. Existing tags default
to Slate. New tags require an explicit color choice after their normalized name is confirmed. Do
not introduce arbitrary per-tag color literals in product render code.

## Subtle surface separators

Use a thin, low-contrast border when adjacent application regions need a persistent boundary but
do not need the visual weight of a new surface. Typical uses include the boundary between a header
or toolbar and scrolling content, the top edge of a status bar, and divisions between flush panels.

These boundaries must use the shared `border.thin` width and `border.muted` color from the Lectern
theme. Define both values for every color mode in the theme layer; do not use component-local pixel
widths, color literals, shadows, or interactive-control borders as substitutes.

Keep separators restrained. Spacing and surface color should establish hierarchy first, so do not
outline every container or card by default. Add a separator where a boundary would otherwise
disappear, and assign the line to only one of the adjacent regions to avoid doubled borders.

## Consistent padding for edge-aligned controls

Controls aligned to an edge or corner of a container must be positioned with deliberate container
padding, not arbitrary per-edge offsets or control-local margins. For a control placed in a simple
header, toolbar, panel, or empty-state boundary, use one spacing token for the container's entire
perimeter:

```text
paddingTop = paddingRight = paddingBottom = paddingLeft
```

The control's visual bounds must therefore have the same inset from each adjacent container edge.
For example, a button aligned to the top-right of a header must not sit nearly flush with the top
while retaining a larger gap on the right. Its top, right, and bottom insets must come from the same
container-padding value, with the equivalent padding preserved on the unused left edge.

Apply asymmetric padding only when a documented layout constraint requires it, such as a safe area,
window control region, or intentionally distinct content hierarchy. In those cases, use named
layout tokens and make the exception explicit. Do not compensate for an incorrectly sized container
or control with independent pixel nudges.

When implementing or reviewing edge-aligned controls, inspect the control's visible bounds rather
than only its layout origin. Borders, shadows, focus rings, and transparent hit-area expansion can
change the perceived inset and must not make otherwise equal spacing appear uneven.

## Concentric radii for nested surfaces

When a rounded surface is nested inside another rounded surface with a uniform inset, padding, or
gap, the inner surface must use a concentric corner radius.

Use:

```text
innerRadius = max(outerRadius - inset, 0)
```

For example, if the outer container has a `16px` corner radius and the inner surface is inset by
`8px`, the inner surface must use an `8px` corner radius.

Do not blindly reuse the parent's corner radius on the child. Doing so produces non-concentric
corners and visually uneven spacing around the curve.

Apply this rule when:

- the inner and outer surfaces visually follow the same rounded corner;
- the inset between them is uniform; or
- cards, panels, buttons, controls, media, or other rounded surfaces are nested inside a rounded
  container and share its corner geometry.

Do not apply the rule merely because one component is rendered inside another. Components with
independent shapes or intentionally different corner geometry may use their own radius.

When implementing or reviewing nested rounded UI, check the relationship between the outer radius
and the actual inset rather than choosing each radius independently. Store shared radius and inset
values as typed theme or component tokens where appropriate; do not recompute stable geometry on a
rendering hot path.
