//! Primer-derived native UI components with Lectern-owned visual identity.

mod assets;
mod brand;
mod components;
mod generated;
mod icon;
mod theme;

pub use assets::LecternAssets;
pub use components::{Button, ButtonSize, ButtonVariant};
pub use icon::TablerIcon;
pub use theme::{ColorMode, PrimerTheme, install_theme};

/// Exact upstream source identities compiled into this generated UI layer.
pub const PRIMER_SOURCE_SUMMARY: &str = generated::primitive_metadata::SOURCE_SUMMARY;
