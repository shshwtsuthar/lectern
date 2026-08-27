# Color swatch native contract

`ColorSwatch` is a circular choice control for the Appearance dialog. The application supplies a
stable ID, visible color name, theme-resolved fill, selected state, and activation handler. The
component exposes the name and selected status to assistive technology; callers must also render
the visible name because hue is never the sole identifier.

The circle is 2 rem with a full radius. Its unselected outline uses the muted border, while the
selected and focus-visible outlines use the active accent. A check mark remains visible inside the
selected circle, so selection is never communicated by color alone. Hover and active opacity shifts
do not change geometry. The component accepts no arbitrary palette map and performs no persistence.
