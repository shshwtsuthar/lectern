use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};

const UPLOAD_ICON: &[u8] = include_bytes!("../assets/tabler/upload.svg");

/// Static UI assets embedded in the Lectern executable.
#[derive(Clone, Copy, Debug, Default)]
pub struct LecternAssets;

impl AssetSource for LecternAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(match path {
            "tabler/upload.svg" => Some(Cow::Borrowed(UPLOAD_ICON)),
            _ => None,
        })
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(match path.trim_matches('/') {
            "" => vec![SharedString::new_static("tabler")],
            "tabler" => vec![SharedString::new_static("upload.svg")],
            _ => Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_only_allowlisted_assets() {
        let assets = LecternAssets;
        assert!(assets.load("tabler/upload.svg").unwrap().is_some());
        assert!(assets.load("tabler/brand-github.svg").unwrap().is_none());
    }
}
