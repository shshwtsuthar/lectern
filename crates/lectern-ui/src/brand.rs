//! Lectern-owned visual identity tokens layered over the derived Primer system.

/// Lectern's canonical mauve brand color (`#9B6AA6`).
pub(crate) const MAUVE: u32 = 0x9b6a_a6ff;

/// Lectern's supporting light lavender (`#D8C4E1`).
pub(crate) const LAVENDER: u32 = 0xd8c4_e1ff;

/// One complete, theme-specific primary-button color contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PrimaryButtonTokens {
    pub(crate) background: u32,
    pub(crate) hover_background: u32,
    pub(crate) active_background: u32,
    pub(crate) disabled_background: u32,
    pub(crate) border: u32,
    pub(crate) hover_border: u32,
    pub(crate) active_border: u32,
    pub(crate) disabled_border: u32,
    pub(crate) foreground: u32,
    pub(crate) disabled_foreground: u32,
    pub(crate) icon: u32,
    pub(crate) disabled_icon: u32,
}

/// Deeper mauve fills retain white-label contrast against light application surfaces.
pub(crate) const LIGHT_PRIMARY_BUTTON: PrimaryButtonTokens = PrimaryButtonTokens {
    background: 0x9560_9fff,
    hover_background: 0x8f59_9aff,
    active_background: 0x8752_8fff,
    disabled_background: LAVENDER,
    border: 0x6f3f_78ff,
    hover_border: 0x6736_6fff,
    active_border: 0x5f2e_67ff,
    disabled_border: 0xbba4_c2ff,
    foreground: 0xffff_ffff,
    disabled_foreground: 0x5948_5eff,
    icon: 0xffff_ffff,
    disabled_icon: 0x5948_5eff,
};

/// Luminous mauve fills use dark labels to remain clear against dark application surfaces.
pub(crate) const DARK_PRIMARY_BUTTON: PrimaryButtonTokens = PrimaryButtonTokens {
    background: MAUVE,
    hover_background: 0xa878_b3ff,
    active_background: 0xb587_c0ff,
    disabled_background: 0x4936_4dff,
    border: 0xd8c4_e159,
    hover_border: 0xd8c4_e173,
    active_border: 0xd8c4_e18c,
    disabled_border: 0xd8c4_e126,
    foreground: 0x0d11_17ff,
    disabled_foreground: 0xad93_b4ff,
    icon: 0x0d11_17ff,
    disabled_icon: 0xad93_b4ff,
};

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMUM_NORMAL_TEXT_CONTRAST: f64 = 4.5;

    fn relative_luminance(color: u32) -> f64 {
        assert_eq!(color & 0xff, 0xff, "contrast inputs must be opaque");
        let channel = |shift: u32| {
            let srgb = f64::from((color >> shift) & 0xff_u32) / 255.0;
            if srgb <= 0.04045 {
                srgb / 12.92
            } else {
                ((srgb + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(24) + 0.7152 * channel(16) + 0.0722 * channel(8)
    }

    fn contrast_ratio(first: u32, second: u32) -> f64 {
        let first = relative_luminance(first);
        let second = relative_luminance(second);
        (first.max(second) + 0.05) / (first.min(second) + 0.05)
    }

    #[test]
    fn enabled_primary_button_states_meet_normal_text_contrast() {
        for (name, tokens) in [
            ("light", LIGHT_PRIMARY_BUTTON),
            ("dark", DARK_PRIMARY_BUTTON),
        ] {
            for (state, background) in [
                ("rest", tokens.background),
                ("hover", tokens.hover_background),
                ("active", tokens.active_background),
            ] {
                let ratio = contrast_ratio(background, tokens.foreground);
                assert!(
                    ratio >= MINIMUM_NORMAL_TEXT_CONTRAST,
                    "{name} {state} contrast {ratio:.3} is below {MINIMUM_NORMAL_TEXT_CONTRAST}"
                );
            }
        }
    }
}
