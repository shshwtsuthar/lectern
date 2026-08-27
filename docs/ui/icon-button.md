# Icon button native contract

`IconButton` is a compact square utility control backed by the same `gpui-base` button behavior,
theme states, focus treatment, and small/medium/large geometry as `Button`. It always requires an
explicit accessible label because its vendored Tabler icon is decorative.

The first call site is the eye-icon action that reveals a referenced book file in its platform file
manager. The external path is intentionally absent from the panel; the icon's accessible name is
**Show file in folder**. Contributor reordering uses the same component with accessible
**Move contributor up/down** labels and Tabler chevrons. Only committed, allowlisted outline icons
may be added through `cargo xtask
primer-sync`; components use the generated static path and never format or load an arbitrary SVG at
render time.

The main toolbar uses the Tabler palette icon with the accessible name **Choose theme and accent
color**. It opens the Appearance dialog immediately to the left of **Select books**.
