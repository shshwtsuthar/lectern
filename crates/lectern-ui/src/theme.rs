use std::{
    fmt,
    sync::{Arc, OnceLock},
};

use gpui::{App, FontWeight, Global, Hsla, Rems, rems, rgba};

use crate::{
    brand::{
        DARK_ACCENT_FOCUS, DARK_ACCENT_PRIMARY, DARK_ACCENT_SELECTION, DARK_TAG_COLORS,
        DIALOG_BACKDROP, DIALOG_BACKGROUND_CONTENT_OPACITY, LIGHT_ACCENT_FOCUS,
        LIGHT_ACCENT_PRIMARY, LIGHT_ACCENT_SELECTION, LIGHT_TAG_COLORS,
    },
    generated::{dark, light, primitive_metadata as common},
};

/// A supported immutable Lectern color mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorMode {
    /// Lectern's light theme.
    Light,
    /// Lectern's dark theme.
    Dark,
}

impl ColorMode {
    /// Every supported mode in presentation order.
    pub const ALL: [Self; 2] = [Self::Light, Self::Dark];

    /// Returns the durable settings value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// Parses a durable settings value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }
}

impl fmt::Display for ColorMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
        })
    }
}

/// A user-selectable Lectern accent used for primary actions, focus, and selection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AccentColor {
    /// Lectern's canonical brand mauve.
    #[default]
    Mauve,
    /// Neutral slate.
    Slate,
    /// Warm coral.
    Coral,
    /// Warm amber.
    Amber,
    /// Fresh mint.
    Mint,
    /// Clear azure.
    Azure,
    /// Soft lilac.
    Lilac,
}

impl AccentColor {
    /// Every supported accent in presentation order.
    pub const ALL: [Self; 7] = [
        Self::Mauve,
        Self::Slate,
        Self::Coral,
        Self::Amber,
        Self::Mint,
        Self::Azure,
        Self::Lilac,
    ];

    /// Returns the stable array offset used by immutable token tables.
    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Mauve => 0,
            Self::Slate => 1,
            Self::Coral => 2,
            Self::Amber => 3,
            Self::Mint => 4,
            Self::Azure => 5,
            Self::Lilac => 6,
        }
    }

    /// Returns the durable settings value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mauve => "mauve",
            Self::Slate => "slate",
            Self::Coral => "coral",
            Self::Amber => "amber",
            Self::Mint => "mint",
            Self::Azure => "azure",
            Self::Lilac => "lilac",
        }
    }

    /// Parses a durable settings value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|accent| accent.as_str() == value)
    }
}

impl fmt::Display for AccentColor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Mauve => "Mauve",
            Self::Slate => "Slate",
            Self::Coral => "Coral",
            Self::Amber => "Amber",
            Self::Mint => "Mint",
            Self::Azure => "Azure",
            Self::Lilac => "Lilac",
        })
    }
}

/// Colors used by the first Lectern empty-library screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceColors {
    /// Main application background.
    pub background: Hsla,
    /// Muted surface background.
    pub muted_background: Hsla,
    /// Primary text.
    pub foreground: Hsla,
    /// Supporting text.
    pub muted_foreground: Hsla,
}

/// Borders used to separate adjacent, non-interactive application surfaces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorderTheme {
    /// Low-contrast border for quiet surface boundaries.
    pub muted: Hsla,
    /// Standard thin border width.
    pub thin: Rems,
}

/// Lectern-branded visual state for selected library content.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SelectionTheme {
    /// Quiet selected-card background.
    pub background: Hsla,
    /// Selected-card and checkbox border.
    pub border: Hsla,
    /// Checked indicator background.
    pub check_background: Hsla,
    /// Checked indicator foreground.
    pub check_foreground: Hsla,
}

/// Modal surface presentation shared by product confirmations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DialogTheme {
    /// Viewport scrim behind the modal surface.
    pub backdrop: Hsla,
    /// Reduced contrast for application content beneath the modal.
    pub background_content_opacity: f32,
    /// Dialog surface corner radius.
    pub radius: Rems,
}

/// Compact anchored-menu surface and row presentation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActionMenuTheme {
    /// Menu surface background.
    pub background: Hsla,
    /// Menu outline.
    pub border: Hsla,
    /// Hovered item background.
    pub hover_background: Hsla,
    /// Selected item background.
    pub selected_background: Hsla,
    /// Outer menu radius.
    pub radius: Rems,
    /// Concentric item radius after the standard small inset.
    pub item_radius: Rems,
}

/// Theme-resolved dots used by the named tag palette.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TagPaletteTheme {
    /// Neutral slate.
    pub slate: Hsla,
    /// Warm coral.
    pub coral: Hsla,
    /// Warm amber.
    pub amber: Hsla,
    /// Fresh mint.
    pub mint: Hsla,
    /// Clear azure.
    pub azure: Hsla,
    /// Soft lilac.
    pub lilac: Hsla,
}

/// Lectern-owned colors for the interactive personal book rating.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RatingTheme {
    /// Filled star foreground using the selected Lectern accent.
    pub filled: Hsla,
    /// Empty star outline.
    pub empty: Hsla,
    /// Star foreground while editing is unavailable.
    pub disabled: Hsla,
}

/// A complete visual state for one Button variant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ButtonColors {
    /// Rest background.
    pub background: Hsla,
    /// Hover background.
    pub hover_background: Hsla,
    /// Active background.
    pub active_background: Hsla,
    /// Disabled background.
    pub disabled_background: Hsla,
    /// Rest border.
    pub border: Hsla,
    /// Hover border.
    pub hover_border: Hsla,
    /// Active border.
    pub active_border: Hsla,
    /// Disabled border.
    pub disabled_border: Hsla,
    /// Rest label color.
    pub foreground: Hsla,
    /// Hover label color.
    pub hover_foreground: Hsla,
    /// Active label color.
    pub active_foreground: Hsla,
    /// Disabled label color.
    pub disabled_foreground: Hsla,
    /// Rest icon color.
    pub icon: Hsla,
    /// Disabled icon color.
    pub disabled_icon: Hsla,
}

/// Button presentation shared across variants.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ButtonTheme {
    /// Default Button colors.
    pub default: ButtonColors,
    /// Primary Button colors.
    pub primary: ButtonColors,
    /// Destructive Button colors.
    pub danger: ButtonColors,
    /// Corner radius.
    pub radius: Rems,
    /// Border width.
    pub border_width: Rems,
}

/// Text-entry colors and geometry shared by single- and multi-line fields.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputTheme {
    /// Rest background.
    pub background: Hsla,
    /// Disabled background.
    pub disabled_background: Hsla,
    /// Rest border.
    pub border: Hsla,
    /// Disabled border.
    pub disabled_border: Hsla,
    /// Entered text.
    pub foreground: Hsla,
    /// Placeholder text.
    pub placeholder: Hsla,
    /// Disabled text.
    pub disabled_foreground: Hsla,
    /// Corner radius.
    pub radius: Rems,
    /// Border width.
    pub border_width: Rems,
    /// Standard single-line field height.
    pub height: Rems,
    /// Inline field padding.
    pub padding_inline: Rems,
}

/// Focus-visible ring presentation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FocusTheme {
    /// Ring color.
    pub color: Hsla,
    /// Ring width.
    pub width: Rems,
    /// Ring offset relative to the control edge.
    pub offset: Rems,
}

/// Typography needed by the bootstrap UI.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TypographyTheme {
    /// Default application font family.
    pub body_family: &'static str,
    /// Font family reserved for the Lectern wordmark.
    pub wordmark_family: &'static str,
    /// Lectern wordmark weight.
    pub wordmark_weight: FontWeight,
    /// Compact line height for centered library-card metadata.
    pub book_metadata_line_height: f32,
    /// Body text size.
    pub body_size: Rems,
    /// Body text line height multiplier.
    pub body_line_height: f32,
    /// Body text weight.
    pub body_weight: FontWeight,
    /// Button label weight.
    pub button_weight: FontWeight,
    /// Heading size.
    pub title_size: Rems,
    /// Heading line height multiplier.
    pub title_line_height: f32,
    /// Heading weight.
    pub title_weight: FontWeight,
}

/// Spacing used by the bootstrap UI.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpacingTheme {
    /// Small space.
    pub small: Rems,
    /// Medium space.
    pub medium: Rems,
    /// Large space.
    pub large: Rems,
    /// Extra-large space.
    pub extra_large: Rems,
}

/// One complete, immutable Lectern theme built from Primer-derived and Lectern-owned tokens.
#[derive(Clone, Debug, PartialEq)]
pub struct PrimerTheme {
    mode: ColorMode,
    accent: AccentColor,
    /// Surface and text colors.
    pub surface: SurfaceColors,
    /// Non-interactive surface borders.
    pub border: BorderTheme,
    /// Selected-content presentation.
    pub selection: SelectionTheme,
    /// Modal surface presentation.
    pub dialog: DialogTheme,
    /// Compact anchored menu presentation.
    pub action_menu: ActionMenuTheme,
    /// Named tag-dot palette.
    pub tag_palette: TagPaletteTheme,
    /// Interactive personal-rating presentation.
    pub rating: RatingTheme,
    /// Button colors and geometry.
    pub button: ButtonTheme,
    /// Text-entry colors and geometry.
    pub input: InputTheme,
    /// Focus-visible ring.
    pub focus: FocusTheme,
    /// Typography.
    pub typography: TypographyTheme,
    /// General spacing.
    pub spacing: SpacingTheme,
}

#[derive(Clone)]
struct CurrentPrimerTheme(Arc<PrimerTheme>);

impl Global for CurrentPrimerTheme {}

impl PrimerTheme {
    /// Returns the process-wide immutable light theme.
    #[must_use]
    pub fn light() -> Arc<Self> {
        static THEME: OnceLock<Arc<PrimerTheme>> = OnceLock::new();
        Arc::clone(
            THEME.get_or_init(|| {
                Arc::new(Self::from_generated(ColorMode::Light, AccentColor::Mauve))
            }),
        )
    }

    /// Returns the process-wide immutable dark theme.
    #[must_use]
    pub fn dark() -> Arc<Self> {
        static THEME: OnceLock<Arc<PrimerTheme>> = OnceLock::new();
        Arc::clone(
            THEME.get_or_init(|| {
                Arc::new(Self::from_generated(ColorMode::Dark, AccentColor::Mauve))
            }),
        )
    }

    /// Builds one immutable theme for a color mode and accent choice.
    #[must_use]
    pub fn with_accent(mode: ColorMode, accent: AccentColor) -> Arc<Self> {
        match (mode, accent) {
            (ColorMode::Light, AccentColor::Mauve) => Self::light(),
            (ColorMode::Dark, AccentColor::Mauve) => Self::dark(),
            _ => Arc::new(Self::from_generated(mode, accent)),
        }
    }

    /// Returns the theme's color mode.
    #[must_use]
    pub const fn mode(&self) -> ColorMode {
        self.mode
    }

    /// Returns the theme's selected accent.
    #[must_use]
    pub const fn accent(&self) -> AccentColor {
        self.accent
    }

    /// Resolves one accent to its visible circular-swatch color in the current mode.
    #[must_use]
    pub fn accent_swatch(&self, accent: AccentColor) -> Hsla {
        if accent == AccentColor::Mauve {
            return rgba(crate::brand::MAUVE).into();
        }
        match accent {
            AccentColor::Mauve => unreachable!("mauve returned above"),
            AccentColor::Slate => self.tag_palette.slate,
            AccentColor::Coral => self.tag_palette.coral,
            AccentColor::Amber => self.tag_palette.amber,
            AccentColor::Mint => self.tag_palette.mint,
            AccentColor::Azure => self.tag_palette.azure,
            AccentColor::Lilac => self.tag_palette.lilac,
        }
    }

    /// Returns the installed theme, falling back to the immutable light theme.
    #[must_use]
    pub fn current(cx: &App) -> Arc<Self> {
        cx.try_global::<CurrentPrimerTheme>()
            .map_or_else(Self::light, |theme| Arc::clone(&theme.0))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one exhaustive constructor makes every immutable theme field visibly complete"
    )]
    fn from_generated(mode: ColorMode, accent: AccentColor) -> Self {
        macro_rules! choose {
            ($name:ident) => {
                match mode {
                    ColorMode::Light => light::$name,
                    ColorMode::Dark => dark::$name,
                }
            };
        }
        let color = |value| rgba(value).into();
        let default_foreground = color(choose!(BUTTON_DEFAULT_FG_REST));
        let primary = match mode {
            ColorMode::Light => LIGHT_ACCENT_PRIMARY[accent.index()],
            ColorMode::Dark => DARK_ACCENT_PRIMARY[accent.index()],
        };
        let selection = match mode {
            ColorMode::Light => LIGHT_ACCENT_SELECTION[accent.index()],
            ColorMode::Dark => DARK_ACCENT_SELECTION[accent.index()],
        };
        let tag_colors = match mode {
            ColorMode::Light => LIGHT_TAG_COLORS,
            ColorMode::Dark => DARK_TAG_COLORS,
        };
        let focus_color = match mode {
            ColorMode::Light => LIGHT_ACCENT_FOCUS[accent.index()],
            ColorMode::Dark => DARK_ACCENT_FOCUS[accent.index()],
        };
        let _upstream_focus_color = choose!(FOCUS_OUTLINE_COLOR);
        Self {
            mode,
            accent,
            surface: SurfaceColors {
                background: color(choose!(BG_COLOR_DEFAULT)),
                muted_background: color(choose!(BG_COLOR_MUTED)),
                foreground: color(choose!(FG_COLOR_DEFAULT)),
                muted_foreground: color(choose!(FG_COLOR_MUTED)),
            },
            border: BorderTheme {
                muted: color(choose!(BORDER_COLOR_MUTED)),
                thin: rems(common::BORDER_WIDTH_THIN),
            },
            selection: SelectionTheme {
                background: color(selection.background),
                border: color(selection.border),
                check_background: color(selection.check_background),
                check_foreground: color(selection.check_foreground),
            },
            dialog: DialogTheme {
                backdrop: color(DIALOG_BACKDROP),
                background_content_opacity: DIALOG_BACKGROUND_CONTENT_OPACITY,
                radius: rems(common::BORDER_RADIUS_LARGE),
            },
            action_menu: ActionMenuTheme {
                background: color(choose!(BG_COLOR_DEFAULT)),
                border: color(choose!(BORDER_COLOR_MUTED)),
                hover_background: color(choose!(BUTTON_DEFAULT_BG_HOVER)),
                selected_background: color(selection.background),
                radius: rems(common::BORDER_RADIUS_LARGE),
                item_radius: rems(common::BORDER_RADIUS_LARGE - common::SPACE_SM),
            },
            tag_palette: TagPaletteTheme {
                slate: color(tag_colors.slate),
                coral: color(tag_colors.coral),
                amber: color(tag_colors.amber),
                mint: color(tag_colors.mint),
                azure: color(tag_colors.azure),
                lilac: color(tag_colors.lilac),
            },
            rating: RatingTheme {
                filled: color(primary.background),
                empty: color(choose!(CONTROL_BORDER_REST)),
                disabled: color(choose!(CONTROL_FG_DISABLED)),
            },
            button: ButtonTheme {
                default: ButtonColors {
                    background: color(choose!(BUTTON_DEFAULT_BG_REST)),
                    hover_background: color(choose!(BUTTON_DEFAULT_BG_HOVER)),
                    active_background: color(choose!(BUTTON_DEFAULT_BG_ACTIVE)),
                    disabled_background: color(choose!(BUTTON_DEFAULT_BG_DISABLED)),
                    border: color(choose!(BUTTON_DEFAULT_BORDER_REST)),
                    hover_border: color(choose!(BUTTON_DEFAULT_BORDER_HOVER)),
                    active_border: color(choose!(BUTTON_DEFAULT_BORDER_ACTIVE)),
                    disabled_border: color(choose!(BUTTON_DEFAULT_BORDER_DISABLED)),
                    foreground: default_foreground,
                    hover_foreground: default_foreground,
                    active_foreground: default_foreground,
                    disabled_foreground: color(choose!(BUTTON_DEFAULT_FG_DISABLED)),
                    icon: default_foreground,
                    disabled_icon: color(choose!(BUTTON_DEFAULT_FG_DISABLED)),
                },
                primary: ButtonColors {
                    background: color(primary.background),
                    hover_background: color(primary.hover_background),
                    active_background: color(primary.active_background),
                    disabled_background: color(primary.disabled_background),
                    border: color(primary.border),
                    hover_border: color(primary.hover_border),
                    active_border: color(primary.active_border),
                    disabled_border: color(primary.disabled_border),
                    foreground: color(primary.foreground),
                    hover_foreground: color(primary.foreground),
                    active_foreground: color(primary.foreground),
                    disabled_foreground: color(primary.disabled_foreground),
                    icon: color(primary.icon),
                    disabled_icon: color(primary.disabled_icon),
                },
                danger: ButtonColors {
                    background: color(choose!(BUTTON_DANGER_BG_REST)),
                    hover_background: color(choose!(BUTTON_DANGER_BG_HOVER)),
                    active_background: color(choose!(BUTTON_DANGER_BG_ACTIVE)),
                    disabled_background: color(choose!(BUTTON_DANGER_BG_DISABLED)),
                    border: color(choose!(BUTTON_DANGER_BORDER_REST)),
                    hover_border: color(choose!(BUTTON_DANGER_BORDER_HOVER)),
                    active_border: color(choose!(BUTTON_DANGER_BORDER_ACTIVE)),
                    disabled_border: color(choose!(BUTTON_DEFAULT_BORDER_DISABLED)),
                    foreground: color(choose!(BUTTON_DANGER_FG_REST)),
                    hover_foreground: color(choose!(BUTTON_DANGER_FG_HOVER)),
                    active_foreground: color(choose!(BUTTON_DANGER_FG_ACTIVE)),
                    disabled_foreground: color(choose!(BUTTON_DANGER_FG_DISABLED)),
                    icon: color(choose!(BUTTON_DANGER_FG_REST)),
                    disabled_icon: color(choose!(BUTTON_DANGER_FG_DISABLED)),
                },
                radius: rems(common::BORDER_RADIUS_MEDIUM),
                border_width: rems(common::BORDER_WIDTH_THIN),
            },
            input: InputTheme {
                background: color(choose!(CONTROL_BG_REST)),
                disabled_background: color(choose!(CONTROL_BG_DISABLED)),
                border: color(choose!(CONTROL_BORDER_REST)),
                disabled_border: color(choose!(CONTROL_BORDER_DISABLED)),
                foreground: color(choose!(CONTROL_FG_REST)),
                placeholder: color(choose!(CONTROL_FG_PLACEHOLDER)),
                disabled_foreground: color(choose!(CONTROL_FG_DISABLED)),
                radius: rems(common::BORDER_RADIUS_MEDIUM),
                border_width: rems(common::BORDER_WIDTH_THIN),
                height: rems(common::CONTROL_MEDIUM_SIZE),
                padding_inline: rems(common::CONTROL_MEDIUM_PADDING_INLINE),
            },
            focus: FocusTheme {
                color: color(focus_color),
                width: rems(common::FOCUS_OUTLINE_WIDTH),
                offset: rems(common::FOCUS_OUTLINE_OFFSET),
            },
            typography: TypographyTheme {
                body_family: "Karla",
                wordmark_family: "Newsreader 14pt",
                wordmark_weight: FontWeight::MEDIUM,
                book_metadata_line_height: 1.25,
                body_size: rems(common::TEXT_BODY_SIZE_MEDIUM),
                body_line_height: common::TEXT_BODY_LINE_HEIGHT_MEDIUM,
                body_weight: FontWeight(f32::from(common::TEXT_BODY_WEIGHT)),
                button_weight: FontWeight(f32::from(common::TEXT_WEIGHT_SEMIBOLD)),
                title_size: rems(common::TEXT_TITLE_SIZE_MEDIUM),
                title_line_height: common::TEXT_TITLE_LINE_HEIGHT_MEDIUM,
                title_weight: FontWeight(f32::from(common::TEXT_TITLE_WEIGHT_MEDIUM)),
            },
            spacing: SpacingTheme {
                small: rems(common::SPACE_SM),
                medium: rems(common::SPACE_MD),
                large: rems(common::SPACE_LG),
                extra_large: rems(common::SPACE_XL),
            },
        }
    }
}

/// Installs one immutable Primer theme in GPUI typed global state.
pub fn install_theme(cx: &mut App, theme: Arc<PrimerTheme>) {
    cx.set_global(CurrentPrimerTheme(theme));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_singletons_are_stable_and_complete() {
        let light = PrimerTheme::light();
        let same_light = PrimerTheme::light();
        let dark = PrimerTheme::dark();

        assert!(Arc::ptr_eq(&light, &same_light));
        assert_eq!(light.mode(), ColorMode::Light);
        assert_eq!(dark.mode(), ColorMode::Dark);
        assert_ne!(light.surface.background, dark.surface.background);
        assert_eq!(light.border.muted, rgba(light::BORDER_COLOR_MUTED).into());
        assert_eq!(dark.border.muted, rgba(dark::BORDER_COLOR_MUTED).into());
        assert_eq!(light.border.thin, dark.border.thin);
        assert_eq!(light.button.radius, dark.button.radius);
        assert_eq!(light.input.radius, dark.input.radius);
        assert_eq!(light.input.height, dark.input.height);
        assert_eq!(light.input.background, rgba(light::CONTROL_BG_REST).into());
        assert_eq!(dark.input.background, rgba(dark::CONTROL_BG_REST).into());
        assert!(
            (light.dialog.background_content_opacity - DIALOG_BACKGROUND_CONTENT_OPACITY).abs()
                < f32::EPSILON
        );
        assert!(
            (dark.dialog.background_content_opacity - DIALOG_BACKGROUND_CONTENT_OPACITY).abs()
                < f32::EPSILON
        );
        assert_eq!(light.typography.body_family, "Karla");
        assert_eq!(light.typography.wordmark_family, "Newsreader 14pt");
        assert_eq!(light.typography.wordmark_weight, FontWeight::MEDIUM);
        assert!((light.typography.book_metadata_line_height - 1.25).abs() < f32::EPSILON);
        assert_eq!(
            light.button.primary.background,
            rgba(LIGHT_ACCENT_PRIMARY[AccentColor::Mauve.index()].background).into()
        );
        assert_eq!(
            dark.button.primary.background,
            rgba(DARK_ACCENT_PRIMARY[AccentColor::Mauve.index()].background).into()
        );
        assert_eq!(
            light.button.primary.icon,
            rgba(LIGHT_ACCENT_PRIMARY[AccentColor::Mauve.index()].icon).into()
        );
        assert_eq!(
            dark.button.primary.icon,
            rgba(DARK_ACCENT_PRIMARY[AccentColor::Mauve.index()].icon).into()
        );
        assert_eq!(
            light.button.primary.disabled_icon,
            rgba(LIGHT_ACCENT_PRIMARY[AccentColor::Mauve.index()].disabled_icon).into()
        );
        assert_eq!(
            dark.button.primary.disabled_icon,
            rgba(DARK_ACCENT_PRIMARY[AccentColor::Mauve.index()].disabled_icon).into()
        );
        assert_eq!(light.rating.filled, light.button.primary.background);
        assert_eq!(dark.rating.filled, dark.button.primary.background);
        assert_eq!(light.rating.empty, rgba(light::CONTROL_BORDER_REST).into());

        for accent in AccentColor::ALL {
            let themed = PrimerTheme::with_accent(ColorMode::Dark, accent);
            assert_eq!(themed.accent(), accent);
            assert_eq!(
                themed.button.primary.background,
                rgba(DARK_ACCENT_PRIMARY[accent.index()].background).into()
            );
            assert_eq!(themed.rating.filled, themed.button.primary.background);
        }
    }
}
