# Appearance dialog native contract

## Journey and workload

The palette icon immediately to the left of **Select books** opens the compact Appearance modal.
Theme, component, asset, title-bar, and first-frame changes use `ui-bootstrap-regression-v1`; modal
composition is also represented by the compositor-backed confirmation path in
`ui-selection-regression-v1`. Both retain raw release samples and peak RSS.

## Behavior

- The main theme is exactly **Light** or **Dark**.
- The accent is Lectern Mauve, Slate, Coral, Amber, Mint, Azure, or Lilac. Mauve remains the default;
  every color from the tag palette is available without making tag color and application accent the
  same domain value.
- A choice previews immediately. It updates primary actions, focus-visible treatment, and selected
  content together so the interface never mixes two accents; closing the dialog persists the final
  preview once, off the UI thread.
- The current mode and accent are stored in `appearance.json` beside the local library. Persistence
  runs off the UI thread; an absent, malformed, or future settings value safely falls back to Light
  and Mauve.
- **Done**, Escape, an outside click, or the modal lifecycle closes the dialog and saves the final
  choice. There is no separate Apply action.

## Presentation and accessibility

The modal is one restrained surface with large perimeter padding, large gaps between major groups,
and small gaps within the two mode choices. Accent choices use `ColorSwatch`: a filled circle,
visible color name, explicit accessible name, selected state, and a non-color selected outline.
Every interactive state comes from a named light/dark accent token set rather than component-local
color arithmetic.

While any modal is open, the application content beneath it uses a restrained soft-focus treatment:
the shared scrim and a named reduction in background-content contrast soften competing detail while
the dialog remains fully opaque and sharp. This is Lectern's lightweight GPUI fallback for the
framework's lack of per-element backdrop filtering; do not increase the effect into a heavy blur or
apply it to the dialog surface itself.

The application requests server-side window decorations on Linux so GNOME may provide its standard
title bar and window controls above Lectern's compact application bar. The platform can decline this
request where server-side decorations are unavailable.
