//! Reusable desktop infrastructure for Lectern.

#[allow(
    dead_code,
    reason = "the library GPUI binary and legacy binary use different parts of shared editor state"
)]
mod curation;

pub mod export;
pub mod gpui_app;
