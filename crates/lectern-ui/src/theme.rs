use std::sync::{Arc, OnceLock};

use gpui::{App, FontWeight, Global, Hsla, Rems, rems, rgba};

use crate::{
    brand::{DARK_PRIMARY_BUTTON, LIGHT_PRIMARY_BUTTON},
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
    /// Corner radius.
    pub radius: Rems,
    /// Border width.
    pub border_width: Rems,
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
    /// Surface and text colors.
    pub surface: SurfaceColors,
    /// Non-interactive surface borders.
    pub border: BorderTheme,
    /// Button colors and geometry.
    pub button: ButtonTheme,
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
        Arc::clone(THEME.get_or_init(|| Arc::new(Self::from_generated(ColorMode::Light))))
    }

    /// Returns the process-wide immutable dark theme.
    #[must_use]
    pub fn dark() -> Arc<Self> {
        static THEME: OnceLock<Arc<PrimerTheme>> = OnceLock::new();
        Arc::clone(THEME.get_or_init(|| Arc::new(Self::from_generated(ColorMode::Dark))))
    }

    /// Returns the theme's color mode.
    #[must_use]
    pub const fn mode(&self) -> ColorMode {
        self.mode
    }

    /// Returns the installed theme, falling back to the immutable light theme.
    #[must_use]
    pub fn current(cx: &App) -> Arc<Self> {
        cx.try_global::<CurrentPrimerTheme>()
            .map_or_else(Self::light, |theme| Arc::clone(&theme.0))
    }

    fn from_generated(mode: ColorMode) -> Self {
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
            ColorMode::Light => LIGHT_PRIMARY_BUTTON,
            ColorMode::Dark => DARK_PRIMARY_BUTTON,
        };
        Self {
            mode,
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
                    disabled_foreground: color(primary.disabled_foreground),
                    icon: color(primary.icon),
                    disabled_icon: color(primary.disabled_icon),
                },
                radius: rems(common::BORDER_RADIUS_MEDIUM),
                border_width: rems(common::BORDER_WIDTH_THIN),
            },
            focus: FocusTheme {
                color: color(choose!(FOCUS_OUTLINE_COLOR)),
                width: rems(common::FOCUS_OUTLINE_WIDTH),
                offset: rems(common::FOCUS_OUTLINE_OFFSET),
            },
            typography: TypographyTheme {
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
        assert_eq!(
            light.button.primary.background,
            rgba(LIGHT_PRIMARY_BUTTON.background).into()
        );
        assert_eq!(
            dark.button.primary.background,
            rgba(DARK_PRIMARY_BUTTON.background).into()
        );
    }
}
