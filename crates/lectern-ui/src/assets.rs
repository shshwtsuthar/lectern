use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

const CHEVRON_DOWN_ICON: &[u8] = include_bytes!("../assets/tabler/chevron-down.svg");
const CHEVRON_UP_ICON: &[u8] = include_bytes!("../assets/tabler/chevron-up.svg");
const EYE_ICON: &[u8] = include_bytes!("../assets/tabler/eye.svg");
const PALETTE_ICON: &[u8] = include_bytes!("../assets/tabler/palette.svg");
const UPLOAD_ICON: &[u8] = include_bytes!("../assets/tabler/upload.svg");
const NEWSREADER_MEDIUM: &[u8] = include_bytes!("../assets/fonts/Newsreader_14pt-Medium.ttf");

/// Static UI assets embedded in the Lectern executable.
#[derive(Clone, Copy, Debug, Default)]
pub struct LecternAssets;

impl AssetSource for LecternAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(match path {
            "tabler/chevron-down.svg" => Some(Cow::Borrowed(CHEVRON_DOWN_ICON)),
            "tabler/chevron-up.svg" => Some(Cow::Borrowed(CHEVRON_UP_ICON)),
            "tabler/eye.svg" => Some(Cow::Borrowed(EYE_ICON)),
            "tabler/palette.svg" => Some(Cow::Borrowed(PALETTE_ICON)),
            "tabler/upload.svg" => Some(Cow::Borrowed(UPLOAD_ICON)),
            _ => None,
        })
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(match path.trim_matches('/') {
            "" => vec![SharedString::new_static("tabler")],
            "tabler" => vec![
                SharedString::new_static("chevron-down.svg"),
                SharedString::new_static("chevron-up.svg"),
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
        assert!(assets.load("tabler/eye.svg").unwrap().is_some());
        assert!(assets.load("tabler/upload.svg").unwrap().is_some());
        assert!(assets.load("tabler/brand-github.svg").unwrap().is_none());
    }
}
