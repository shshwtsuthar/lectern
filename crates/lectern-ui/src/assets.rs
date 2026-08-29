use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

const CHEVRON_DOWN_ICON: &[u8] = include_bytes!("../assets/tabler/chevron-down.svg");
const CHEVRON_UP_ICON: &[u8] = include_bytes!("../assets/tabler/chevron-up.svg");
const DEVICE_TABLET_ICON: &[u8] = include_bytes!("../assets/tabler/device-tablet.svg");
const EYE_ICON: &[u8] = include_bytes!("../assets/tabler/eye.svg");
const PALETTE_ICON: &[u8] = include_bytes!("../assets/tabler/palette.svg");
const UPLOAD_ICON: &[u8] = include_bytes!("../assets/tabler/upload.svg");
const NEWSREADER_MEDIUM: &[u8] = include_bytes!("../assets/fonts/Newsreader_14pt-Medium.ttf");
const COVER_OVERLAY_A: &[u8] = include_bytes!("../assets/material/cover-overlay-a.svg");
const COVER_OVERLAY_B: &[u8] = include_bytes!("../assets/material/cover-overlay-b.svg");

/// Static UI assets embedded in the Lectern executable.
#[derive(Clone, Copy, Debug, Default)]
pub struct LecternAssets;

impl AssetSource for LecternAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(match path {
            "tabler/chevron-down.svg" => Some(Cow::Borrowed(CHEVRON_DOWN_ICON)),
            "tabler/chevron-up.svg" => Some(Cow::Borrowed(CHEVRON_UP_ICON)),
            "tabler/device-tablet.svg" => Some(Cow::Borrowed(DEVICE_TABLET_ICON)),
            "tabler/eye.svg" => Some(Cow::Borrowed(EYE_ICON)),
            "tabler/palette.svg" => Some(Cow::Borrowed(PALETTE_ICON)),
            "tabler/upload.svg" => Some(Cow::Borrowed(UPLOAD_ICON)),
            "material/cover-overlay-a.svg" => Some(Cow::Borrowed(COVER_OVERLAY_A)),
            "material/cover-overlay-b.svg" => Some(Cow::Borrowed(COVER_OVERLAY_B)),
            _ => None,
        })
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(match path.trim_matches('/') {
            "" => vec![
                SharedString::new_static("material"),
                SharedString::new_static("tabler"),
            ],
            "material" => vec![
                SharedString::new_static("cover-overlay-a.svg"),
                SharedString::new_static("cover-overlay-b.svg"),
            ],
            "tabler" => vec![
                SharedString::new_static("chevron-down.svg"),
                SharedString::new_static("chevron-up.svg"),
                SharedString::new_static("device-tablet.svg"),
                SharedString::new_static("eye.svg"),
                SharedString::new_static("palette.svg"),
                SharedString::new_static("upload.svg"),
            ],
            _ => Vec::new(),
        })
    }
}

/// Installs Lectern's bundled application fonts into GPUI's process text system.
///
/// # Errors
///
/// Returns an error when the platform text system rejects the embedded font data.
pub fn install_fonts(cx: &gpui::App) -> Result<()> {
    cx.text_system()
        .add_fonts(vec![Cow::Borrowed(NEWSREADER_MEDIUM)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_only_allowlisted_assets() {
        let assets = LecternAssets;
        assert!(assets.load("tabler/device-tablet.svg").unwrap().is_some());
        assert!(assets.load("tabler/eye.svg").unwrap().is_some());
        assert!(assets.load("tabler/upload.svg").unwrap().is_some());
        assert!(
            assets
                .load("material/cover-overlay-a.svg")
                .unwrap()
                .is_some()
        );
        assert!(
            assets
                .load("material/cover-overlay-b.svg")
                .unwrap()
                .is_some()
        );
        assert!(assets.load("tabler/brand-github.svg").unwrap().is_none());
    }

    #[test]
    fn bundled_wordmark_font_contains_gpui_loader_glyph() {
        let face = ttf_parser::Face::parse(NEWSREADER_MEDIUM, 0).unwrap();
        assert!(face.names().into_iter().any(|name| {
            name.name_id == ttf_parser::name_id::TYPOGRAPHIC_FAMILY
                && name.to_string().as_deref() == Some("Newsreader 14pt")
        }));
        assert_eq!(face.weight().to_number(), 500);
        for character in "Lecternm".chars() {
            assert!(
                face.glyph_index(character).is_some(),
                "Newsreader subset must contain {character:?}"
            );
        }
    }
}
