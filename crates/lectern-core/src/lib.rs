//! Core application and domain boundary for Lectern.
//!
//! This crate intentionally has no UI or infrastructure dependencies. Product
//! capabilities can grow here behind explicit interfaces while desktop, CLI,
//! storage, and device integrations remain replaceable adapters.

/// Compile-time information about the running Lectern build.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuildInfo {
    /// Human-readable product name.
    pub name: &'static str,
    /// Semantic version supplied by Cargo.
    pub version: &'static str,
}

impl BuildInfo {
    /// Returns information for the currently compiled build.
    #[must_use]
    pub const fn current() -> Self {
        Self {
            name: "Lectern",
            version: env!("CARGO_PKG_VERSION"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BuildInfo;

    #[test]
    fn current_build_info_is_populated() {
        let build = BuildInfo::current();

        assert_eq!(build.name, "Lectern");
        assert!(!build.version.is_empty());
    }
}
