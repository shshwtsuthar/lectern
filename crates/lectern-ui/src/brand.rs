//! Lectern-owned visual identity tokens layered over the derived Primer system.

/// Lectern's canonical mauve brand color (`#9B6AA6`).
pub(crate) const MAUVE: u32 = 0x9b6a_a6ff;

/// Lectern's supporting light lavender (`#D8C4E1`).
pub(crate) const LAVENDER: u32 = 0xd8c4_e1ff;

/// Lectern-owned selected-state colors for one application color mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SelectionTokens {
    pub(crate) background: u32,
    pub(crate) border: u32,
    pub(crate) check_background: u32,
    pub(crate) check_foreground: u32,
}

pub(crate) const LIGHT_SELECTION: SelectionTokens = SelectionTokens {
    background: 0xf6ef_f8ff,
    border: 0x9560_9fff,
    check_background: 0x9560_9fff,
    check_foreground: 0xffff_ffff,
};

pub(crate) const DARK_SELECTION: SelectionTokens = SelectionTokens {
    background: 0x2d22_30ff,
    border: LAVENDER,
    check_background: MAUVE,
    check_foreground: 0x0d11_17ff,
};

/// Scrim used behind modal Lectern surfaces.
pub(crate) const DIALOG_BACKDROP: u32 = 0x0000_0066;

/// Lectern-owned focus outlines replace Primer's blue focus accent.
pub(crate) const LIGHT_FOCUS: u32 = MAUVE;
pub(crate) const DARK_FOCUS: u32 = LAVENDER;

/// Named tag colors for one application color mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TagColorTokens {
    pub(crate) slate: u32,
    pub(crate) coral: u32,
    pub(crate) amber: u32,
    pub(crate) mint: u32,
    pub(crate) azure: u32,
    pub(crate) lilac: u32,
}

/// Clear but restrained tag dots on light surfaces.
pub(crate) const LIGHT_TAG_COLORS: TagColorTokens = TagColorTokens {
    slate: 0x6474_8bff,
    coral: 0xff57_57ff,
    amber: 0xf28c_0fff,
    mint: 0x13ae_91ff,
    azure: 0x337f_efff,
    lilac: 0x8952_f5ff,
};

/// Slightly lifted tag dots that remain legible on dark surfaces.
pub(crate) const DARK_TAG_COLORS: TagColorTokens = TagColorTokens {
    slate: 0x94a3_b8ff,
    coral: 0xff6b_6bff,
    amber: 0xffa9_24ff,
    mint: 0x2dcc_a8ff,
    azure: 0x5b9b_f8ff,
    lilac: 0xa678_f7ff,
};

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

const LIGHT_SLATE_PRIMARY: PrimaryButtonTokens = PrimaryButtonTokens {
    background: 0x5261_76ff,
    hover_background: 0x4857_6bff,
    active_background: 0x3e4d_61ff,
    disabled_background: 0xd8de_e7ff,
    border: 0x3e4d_61ff,
    hover_border: 0x3543_56ff,
    active_border: 0x2d3a_4cff,
    disabled_border: 0xb8c1_ceff,
    foreground: 0xffff_ffff,
    disabled_foreground: 0x6572_83ff,
    icon: 0xffff_ffff,
    disabled_icon: 0x6572_83ff,
};

const LIGHT_CORAL_PRIMARY: PrimaryButtonTokens = PrimaryButtonTokens {
    background: 0xc83c_3cff,
    hover_background: 0xb934_34ff,
    active_background: 0xaa2e_2eff,
    disabled_background: 0xf5c7_c7ff,
    border: 0x9d29_29ff,
    hover_border: 0x9023_23ff,
    active_border: 0x821e_1eff,
    disabled_border: 0xdda6_a6ff,
    foreground: 0xffff_ffff,
    disabled_foreground: 0x7854_54ff,
    icon: 0xffff_ffff,
    disabled_icon: 0x7854_54ff,
};

const LIGHT_AMBER_PRIMARY: PrimaryButtonTokens = PrimaryButtonTokens {
    background: 0x9b56_08ff,
    hover_background: 0x8c4c_06ff,
    active_background: 0x7d43_05ff,
    disabled_background: 0xf1d3_a9ff,
    border: 0x713b_03ff,
    hover_border: 0x6534_02ff,
    active_border: 0x592d_01ff,
    disabled_border: 0xd7b6_86ff,
    foreground: 0xffff_ffff,
    disabled_foreground: 0x725b_3bff,
    icon: 0xffff_ffff,
    disabled_icon: 0x725b_3bff,
};

const LIGHT_MINT_PRIMARY: PrimaryButtonTokens = PrimaryButtonTokens {
    background: 0x087b_69ff,
    hover_background: 0x076f_5fff,
    active_background: 0x0663_55ff,
    disabled_background: 0xbce4_dcff,
    border: 0x055a_4cff,
    hover_border: 0x044f_43ff,
    active_border: 0x0345_3aff,
    disabled_border: 0x94c9_bfff,
    foreground: 0xffff_ffff,
    disabled_foreground: 0x456c_65ff,
    icon: 0xffff_ffff,
    disabled_icon: 0x456c_65ff,
};

const LIGHT_AZURE_PRIMARY: PrimaryButtonTokens = PrimaryButtonTokens {
    background: 0x2867_c5ff,
    hover_background: 0x235c_b5ff,
    active_background: 0x1f52_a5ff,
    disabled_background: 0xc5d7_f3ff,
    border: 0x1c49_94ff,
    hover_border: 0x1840_84ff,
    active_border: 0x1537_74ff,
    disabled_border: 0xa2b9_dcff,
    foreground: 0xffff_ffff,
    disabled_foreground: 0x5265_83ff,
    icon: 0xffff_ffff,
    disabled_icon: 0x5265_83ff,
};

const LIGHT_LILAC_PRIMARY: PrimaryButtonTokens = PrimaryButtonTokens {
    background: 0x7042_c1ff,
    hover_background: 0x6539_b4ff,
    active_background: 0x5a32_a6ff,
    disabled_background: 0xd9c9_f2ff,
    border: 0x512a_97ff,
    hover_border: 0x4824_89ff,
    active_border: 0x401e_7aff,
    disabled_border: 0xbca7_dcff,
    foreground: 0xffff_ffff,
    disabled_foreground: 0x6655_7dff,
    icon: 0xffff_ffff,
    disabled_icon: 0x6655_7dff,
};

const fn dark_accent_primary(
    background: u32,
    hover_background: u32,
    active_background: u32,
    disabled_background: u32,
    disabled_foreground: u32,
) -> PrimaryButtonTokens {
    PrimaryButtonTokens {
        background,
        hover_background,
        active_background,
        disabled_background,
        border: background,
        hover_border: hover_background,
        active_border: active_background,
        disabled_border: disabled_background,
        foreground: 0x0d11_17ff,
        disabled_foreground,
        icon: 0x0d11_17ff,
        disabled_icon: disabled_foreground,
    }
}

const DARK_SLATE_PRIMARY: PrimaryButtonTokens = dark_accent_primary(
    0x94a3_b8ff,
    0xa4b1_c3ff,
    0xb5bf_ceff,
    0x3540_4eff,
    0x8490_a1ff,
);
const DARK_CORAL_PRIMARY: PrimaryButtonTokens = dark_accent_primary(
    0xff6b_6bff,
    0xff7c_7cff,
    0xff8d_8dff,
    0x542f_33ff,
    0xb47d_7dff,
);
const DARK_AMBER_PRIMARY: PrimaryButtonTokens = dark_accent_primary(
    0xffa9_24ff,
    0xffb5_42ff,
    0xffc1_61ff,
    0x543d_1cff,
    0xb792_55ff,
);
const DARK_MINT_PRIMARY: PrimaryButtonTokens = dark_accent_primary(
    0x2dcc_a8ff,
    0x4bd4_b5ff,
    0x69dc_c2ff,
    0x214b_43ff,
    0x73aa_9dff,
);
const DARK_AZURE_PRIMARY: PrimaryButtonTokens = dark_accent_primary(
    0x5b9b_f8ff,
    0x73aa_f9ff,
    0x8bb8_faff,
    0x263d_5eff,
    0x7699_caff,
);
const DARK_LILAC_PRIMARY: PrimaryButtonTokens = dark_accent_primary(
    0xa678_f7ff,
    0xb58d_f8ff,
    0xc4a2_f9ff,
    0x3e31_57ff,
    0x9a83_bfff,
);

/// Primary-action tokens indexed by [`crate::AccentColor`].
pub(crate) const LIGHT_ACCENT_PRIMARY: [PrimaryButtonTokens; 7] = [
    LIGHT_PRIMARY_BUTTON,
    LIGHT_SLATE_PRIMARY,
    LIGHT_CORAL_PRIMARY,
    LIGHT_AMBER_PRIMARY,
    LIGHT_MINT_PRIMARY,
    LIGHT_AZURE_PRIMARY,
    LIGHT_LILAC_PRIMARY,
];

/// Dark primary-action tokens indexed by [`crate::AccentColor`].
pub(crate) const DARK_ACCENT_PRIMARY: [PrimaryButtonTokens; 7] = [
    DARK_PRIMARY_BUTTON,
    DARK_SLATE_PRIMARY,
    DARK_CORAL_PRIMARY,
    DARK_AMBER_PRIMARY,
    DARK_MINT_PRIMARY,
    DARK_AZURE_PRIMARY,
    DARK_LILAC_PRIMARY,
];

/// Selected-content tokens indexed by [`crate::AccentColor`].
pub(crate) const LIGHT_ACCENT_SELECTION: [SelectionTokens; 7] = [
    LIGHT_SELECTION,
    SelectionTokens {
        background: 0xf1f4_f8ff,
        border: 0x5261_76ff,
        check_background: 0x5261_76ff,
        check_foreground: 0xffff_ffff,
    },
    SelectionTokens {
        background: 0xfff0_f0ff,
        border: 0xc83c_3cff,
        check_background: 0xc83c_3cff,
        check_foreground: 0xffff_ffff,
    },
    SelectionTokens {
        background: 0xfff4_e3ff,
        border: 0x9b56_08ff,
        check_background: 0x9b56_08ff,
        check_foreground: 0xffff_ffff,
    },
    SelectionTokens {
        background: 0xe9f8_f4ff,
        border: 0x087b_69ff,
        check_background: 0x087b_69ff,
        check_foreground: 0xffff_ffff,
    },
    SelectionTokens {
        background: 0xedf4_ffff,
        border: 0x2867_c5ff,
        check_background: 0x2867_c5ff,
        check_foreground: 0xffff_ffff,
    },
    SelectionTokens {
        background: 0xf3ef_ffff,
        border: 0x7042_c1ff,
        check_background: 0x7042_c1ff,
        check_foreground: 0xffff_ffff,
    },
];

/// Dark selected-content tokens indexed by [`crate::AccentColor`].
pub(crate) const DARK_ACCENT_SELECTION: [SelectionTokens; 7] = [
    DARK_SELECTION,
    SelectionTokens {
        background: 0x2630_3cff,
        border: 0x94a3_b8ff,
        check_background: 0x94a3_b8ff,
        check_foreground: 0x0d11_17ff,
    },
    SelectionTokens {
        background: 0x3c22_25ff,
        border: 0xff6b_6bff,
        check_background: 0xff6b_6bff,
        check_foreground: 0x0d11_17ff,
    },
    SelectionTokens {
        background: 0x392b_16ff,
        border: 0xffa9_24ff,
        check_background: 0xffa9_24ff,
        check_foreground: 0x0d11_17ff,
    },
    SelectionTokens {
        background: 0x1735_2fff,
        border: 0x2dcc_a8ff,
        check_background: 0x2dcc_a8ff,
        check_foreground: 0x0d11_17ff,
    },
    SelectionTokens {
        background: 0x1d2e_47ff,
        border: 0x5b9b_f8ff,
        check_background: 0x5b9b_f8ff,
        check_foreground: 0x0d11_17ff,
    },
    SelectionTokens {
        background: 0x3026_42ff,
        border: 0xa678_f7ff,
        check_background: 0xa678_f7ff,
        check_foreground: 0x0d11_17ff,
    },
];

/// Focus tokens indexed by [`crate::AccentColor`].
pub(crate) const LIGHT_ACCENT_FOCUS: [u32; 7] = [
    LIGHT_FOCUS,
    0x5261_76ff,
    0xc83c_3cff,
    0x9b56_08ff,
    0x087b_69ff,
    0x2867_c5ff,
    0x7042_c1ff,
];

/// Dark focus tokens indexed by [`crate::AccentColor`].
pub(crate) const DARK_ACCENT_FOCUS: [u32; 7] = [
    DARK_FOCUS,
    0x94a3_b8ff,
    0xff6b_6bff,
    0xffa9_24ff,
    0x2dcc_a8ff,
    0x5b9b_f8ff,
    0xa678_f7ff,
];

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
        for (name, tokens) in LIGHT_ACCENT_PRIMARY
            .into_iter()
            .enumerate()
            .map(|(index, tokens)| (format!("light accent {index}"), tokens))
            .chain(
                DARK_ACCENT_PRIMARY
                    .into_iter()
                    .enumerate()
                    .map(|(index, tokens)| (format!("dark accent {index}"), tokens)),
            )
        {
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

    #[test]
    fn checked_selection_indicators_meet_normal_text_contrast() {
        for (name, tokens) in LIGHT_ACCENT_SELECTION
            .into_iter()
            .enumerate()
            .map(|(index, tokens)| (format!("light accent {index}"), tokens))
            .chain(
                DARK_ACCENT_SELECTION
                    .into_iter()
                    .enumerate()
                    .map(|(index, tokens)| (format!("dark accent {index}"), tokens)),
            )
        {
            let ratio = contrast_ratio(tokens.check_background, tokens.check_foreground);
            assert!(
                ratio >= MINIMUM_NORMAL_TEXT_CONTRAST,
                "{name} checked-indicator contrast {ratio:.3} is below {MINIMUM_NORMAL_TEXT_CONTRAST}"
            );
        }
    }
}
