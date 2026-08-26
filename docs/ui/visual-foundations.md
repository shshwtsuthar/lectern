# Visual foundations

These foundations define Lectern's product-specific visual direction. `lectern-ui` is derived from
Primer, but Lectern is not a Primer replica and must remain visually distinct from GitHub. Primer
supplies useful component structure, interaction patterns, and primitive foundations; it does not
define Lectern's brand.

When Lectern intentionally differs from Primer, represent the difference with an explicitly named
Lectern token rather than disguising it as an imported Primer primitive. Do not inherit a GitHub
brand convention merely because the upstream Primer component uses it.

## Brand color

Lectern's primary brand color is **Lectern Mauve** (`#9B6AA6`). It is a soft, luminous
mauve-lavender chosen to give the application a distinctive, literary identity without reading as a
conventional saturated purple. **Lectern Lavender** (`#D8C4E1`) is its supporting light tint for
restrained accents, highlights, and decorative treatments; it is not a substitute for the primary
color when the brand must be immediately recognizable.

Lectern Mauve replaces the usual GitHub green in brand-primary roles, including primary actions.
Do not use GitHub green as Lectern's default primary-action color. Green may still be used when it
communicates an independent semantic meaning such as success, provided that meaning is not conveyed
by color alone.

Use named Lectern brand tokens when these colors enter production code. Do not scatter hexadecimal
literals through components or relabel the colors as Primer primitives. Interactive, hover, active,
disabled, focus, and theme-specific values must be defined deliberately in the theme layer rather
than calculated ad hoc during rendering.

Brand color does not override semantics or accessibility. Do not use brand color alone to
communicate status, and verify the contrast of the actual foreground/background pair for its text
size and interaction state.

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
