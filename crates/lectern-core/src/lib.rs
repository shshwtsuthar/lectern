//! Core application and domain boundary for Lectern.
//!
//! This crate intentionally has no UI or infrastructure dependencies. Product
//! capabilities can grow here behind explicit interfaces while desktop, CLI,
//! storage, and device integrations remain replaceable adapters.

use std::{fmt, path::PathBuf};

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

/// Stable identifier for a book inside a Lectern library.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BookId(i64);

impl BookId {
    /// Creates an identifier from its database representation.
    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    /// Returns the database representation of this identifier.
    #[must_use]
    pub const fn value(self) -> i64 {
        self.0
    }
}

impl fmt::Display for BookId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// File format owned by a library entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BookFormat {
    /// EPUB publication.
    Epub,
    /// Portable Document Format publication.
    Pdf,
}

impl BookFormat {
    /// All currently supported formats.
    pub const ALL: [Self; 2] = [Self::Epub, Self::Pdf];

    /// Returns the stable lowercase storage value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Epub => "epub",
            Self::Pdf => "pdf",
        }
    }

    /// Parses a stable storage value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "epub" => Some(Self::Epub),
            "pdf" => Some(Self::Pdf),
            _ => None,
        }
    }
}

impl fmt::Display for BookFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Epub => formatter.write_str("EPUB"),
            Self::Pdf => formatter.write_str("PDF"),
        }
    }
}

/// Sort order applied to the library projection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SortOrder {
    /// Sort by normalized title, ascending.
    #[default]
    Title,
    /// Sort by normalized author, then title.
    Author,
    /// Show the most recently added books first.
    RecentlyAdded,
}

impl SortOrder {
    /// All sort orders exposed by the application.
    pub const ALL: [Self; 3] = [Self::Title, Self::Author, Self::RecentlyAdded];
}

impl fmt::Display for SortOrder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Title => formatter.write_str("Title"),
            Self::Author => formatter.write_str("Author"),
            Self::RecentlyAdded => formatter.write_str("Recently added"),
        }
    }
}

/// Search, filter, and sort parameters for a library projection.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LibraryQuery {
    /// User-entered full-text query.
    pub search: String,
    /// Optional file-format filter.
    pub format: Option<BookFormat>,
    /// Requested result order.
    pub sort: SortOrder,
}

/// Compact book data used by the library browser.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BookSummary {
    /// Stable library identifier.
    pub id: BookId,
    /// Display title.
    pub title: String,
    /// Display-ready author names.
    pub authors: String,
    /// Optional series name.
    pub series: Option<String>,
    /// Stored ebook format.
    pub format: BookFormat,
    /// Whether a cached cover thumbnail is available.
    pub has_cover: bool,
}

/// Complete editable metadata for a library entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Book {
    /// Stable library identifier.
    pub id: BookId,
    /// Display title.
    pub title: String,
    /// Display-ready author names.
    pub authors: String,
    /// Optional series name.
    pub series: Option<String>,
    /// Optional publisher.
    pub publisher: Option<String>,
    /// Optional publication language.
    pub language: Option<String>,
    /// Optional description or synopsis.
    pub description: Option<String>,
    /// Stored ebook format.
    pub format: BookFormat,
    /// Original ebook path.
    pub source_path: PathBuf,
}

/// Metadata discovered before a book is inserted into the library.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BookDraft {
    /// Display title.
    pub title: String,
    /// Display-ready author names.
    pub authors: String,
    /// Optional series name.
    pub series: Option<String>,
    /// Optional publisher.
    pub publisher: Option<String>,
    /// Optional publication language.
    pub language: Option<String>,
    /// Optional description or synopsis.
    pub description: Option<String>,
    /// Discovered ebook format.
    pub format: BookFormat,
    /// Original ebook path.
    pub source_path: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::{BookFormat, BookId, BuildInfo};

    #[test]
    fn current_build_info_is_populated() {
        let build = BuildInfo::current();

        assert_eq!(build.name, "Lectern");
        assert!(!build.version.is_empty());
    }

    #[test]
    fn identifiers_round_trip_their_storage_value() {
        let id = BookId::new(42);

        assert_eq!(id.value(), 42);
        assert_eq!(id.to_string(), "42");
    }

    #[test]
    fn formats_have_stable_storage_values() {
        assert_eq!(BookFormat::Epub.as_str(), "epub");
        assert_eq!(BookFormat::Pdf.as_str(), "pdf");
        assert_eq!(BookFormat::parse("epub"), Some(BookFormat::Epub));
        assert_eq!(BookFormat::parse("pdf"), Some(BookFormat::Pdf));
        assert_eq!(BookFormat::parse("mobi"), None);
    }
}
