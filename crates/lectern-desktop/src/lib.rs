//! Reusable desktop infrastructure for Lectern.

#[allow(
    dead_code,
    reason = "shared editor state retains workflow helpers for incremental GPUI interactions"
)]
mod curation;
mod device;

pub mod export;
pub mod gpui_app;
