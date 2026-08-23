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

/// Stable identifier for one file asset owned by a logical book.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AssetId(i64);

impl AssetId {
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

impl fmt::Display for AssetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// File format of a book asset.
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

/// Ownership and path semantics for a book asset.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AssetStorage {
    /// The file remains user-owned at an external path.
    #[default]
    Reference,
    /// The file is owned by Lectern below its managed library root.
    Managed,
}

impl AssetStorage {
    /// Returns the stable lowercase storage value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::Managed => "managed",
        }
    }

    /// Parses a stable storage value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "reference" => Some(Self::Reference),
            "managed" => Some(Self::Managed),
            _ => None,
        }
    }
}

impl fmt::Display for AssetStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reference => formatter.write_str("Referenced"),
            Self::Managed => formatter.write_str("Managed"),
        }
    }
}

/// File asset ready to be attached to a logical book.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BookAssetDraft {
    /// Publication format.
    pub format: BookFormat,
    /// Whether Lectern or the user owns the file.
    pub storage: AssetStorage,
    /// External path for a reference asset or library-relative path for a managed asset.
    pub path: PathBuf,
}

/// One stored file representation of a logical book.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BookAsset {
    /// Stable asset identifier.
    pub id: AssetId,
    /// Publication format.
    pub format: BookFormat,
    /// Whether Lectern or the user owns the file.
    pub storage: AssetStorage,
    /// External path for a reference asset or library-relative path for a managed asset.
    pub path: PathBuf,
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
    /// File representations attached to this logical book.
    pub assets: Vec<BookAsset>,
}

/// Logical-book metadata ready to be inserted into the library.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BookMetadataDraft {
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
}

/// Metadata and location discovered from one publication file.
///
/// New aggregate importers should pair [`BookMetadataDraft`] with one or more
/// [`BookAssetDraft`] values. This single-file shape remains useful for format parsers and
/// compatibility with callers that import independent files.
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
    use super::{AssetId, AssetStorage, BookFormat, BookId, BuildInfo};

    #[test]
    fn current_build_info_is_populated() {
        let build = BuildInfo::current();

        assert_eq!(build.name, "Lectern");
        assert!(!build.version.is_empty());
    }

    #[test]
    fn identifiers_round_trip_their_storage_value() {
        let book_id = BookId::new(42);
        let asset_id = AssetId::new(84);

        assert_eq!(book_id.value(), 42);
        assert_eq!(book_id.to_string(), "42");
        assert_eq!(asset_id.value(), 84);
        assert_eq!(asset_id.to_string(), "84");
    }

    #[test]
    fn formats_have_stable_storage_values() {
        assert_eq!(BookFormat::Epub.as_str(), "epub");
        assert_eq!(BookFormat::Pdf.as_str(), "pdf");
        assert_eq!(BookFormat::parse("epub"), Some(BookFormat::Epub));
        assert_eq!(BookFormat::parse("pdf"), Some(BookFormat::Pdf));
        assert_eq!(BookFormat::parse("mobi"), None);
    }

    #[test]
    fn storage_modes_have_stable_values() {
        assert_eq!(AssetStorage::Reference.as_str(), "reference");
        assert_eq!(AssetStorage::Managed.as_str(), "managed");
        assert_eq!(
            AssetStorage::parse("reference"),
            Some(AssetStorage::Reference)
        );
        assert_eq!(AssetStorage::parse("managed"), Some(AssetStorage::Managed));
        assert_eq!(AssetStorage::parse("copied"), None);
    }
}
