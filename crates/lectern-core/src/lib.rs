//! Core application and domain boundary for Lectern.
//!
//! This crate intentionally has no UI or infrastructure dependencies. Product
//! capabilities can grow here behind explicit interfaces while desktop, CLI,
//! storage, and device integrations remain replaceable adapters.

pub mod organisation;

use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
};

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

/// Calendar date associated with a canonical book's publication.
///
/// Year-only and year-month values are supported because publication metadata is frequently less
/// precise than a complete calendar date. The durable representation is ISO 8601 `YYYY`,
/// `YYYY-MM`, or `YYYY-MM-DD`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PublicationDate {
    year: u16,
    month: Option<u8>,
    day: Option<u8>,
}

impl PublicationDate {
    /// Returns the publication year.
    #[must_use]
    pub const fn year(self) -> u16 {
        self.year
    }

    /// Returns the publication month when recorded.
    #[must_use]
    pub const fn month(self) -> Option<u8> {
        self.month
    }

    /// Returns the publication day when recorded.
    #[must_use]
    pub const fn day(self) -> Option<u8> {
        self.day
    }
}

impl FromStr for PublicationDate {
    type Err = PublicationDateError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let bytes = value.as_bytes();
        if !matches!(bytes.len(), 4 | 7 | 10)
            || bytes.get(4).is_some_and(|separator| *separator != b'-')
            || bytes.get(7).is_some_and(|separator| *separator != b'-')
        {
            return Err(PublicationDateError);
        }

        let year = parse_date_digits(&bytes[..4]).ok_or(PublicationDateError)?;
        if year == 0 {
            return Err(PublicationDateError);
        }
        if bytes.len() == 4 {
            return Ok(Self {
                year,
                month: None,
                day: None,
            });
        }

        let month = u8::try_from(parse_date_digits(&bytes[5..7]).ok_or(PublicationDateError)?)
            .map_err(|_| PublicationDateError)?;
        if !(1..=12).contains(&month) {
            return Err(PublicationDateError);
        }
        if bytes.len() == 7 {
            return Ok(Self {
                year,
                month: Some(month),
                day: None,
            });
        }

        let day = u8::try_from(parse_date_digits(&bytes[8..10]).ok_or(PublicationDateError)?)
            .map_err(|_| PublicationDateError)?;
        let maximum_day = days_in_month(year, month);
        if !(1..=maximum_day).contains(&day) {
            return Err(PublicationDateError);
        }
        Ok(Self {
            year,
            month: Some(month),
            day: Some(day),
        })
    }
}

impl fmt::Display for PublicationDate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:04}", self.year)?;
        if let Some(month) = self.month {
            write!(formatter, "-{month:02}")?;
        }
        if let Some(day) = self.day {
            write!(formatter, "-{day:02}")?;
        }
        Ok(())
    }
}

/// Error returned when publication date text is not a real ISO calendar date.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationDateError;

impl fmt::Display for PublicationDateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("publication date must be YYYY, YYYY-MM, or YYYY-MM-DD")
    }
}

impl Error for PublicationDateError {}

fn parse_date_digits(bytes: &[u8]) -> Option<u16> {
    bytes.iter().try_fold(0_u16, |value, byte| {
        byte.is_ascii_digit()
            .then(|| value * 10 + u16::from(*byte - b'0'))
    })
}

const fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(400) || (year.is_multiple_of(4) && !year.is_multiple_of(100)) => {
            29
        }
        2 => 28,
        _ => 31,
    }
}

/// A canonical zero-to-five-star book rating stored in exact half-star steps.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BookRating(u8);

impl BookRating {
    /// Maximum durable rating in half-star units.
    pub const MAX_HALF_STARS: u8 = 10;

    /// Builds a rating from exact half-star units (`0..=10`).
    #[must_use]
    pub const fn from_half_stars(half_stars: u8) -> Option<Self> {
        if half_stars <= Self::MAX_HALF_STARS {
            Some(Self(half_stars))
        } else {
            None
        }
    }

    /// Returns the exact durable half-star count.
    #[must_use]
    pub const fn half_stars(self) -> u8 {
        self.0
    }
}

impl fmt::Display for BookRating {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_multiple_of(2) {
            write!(formatter, "{}", self.0 / 2)
        } else {
            write!(formatter, "{}.5", self.0 / 2)
        }
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

/// Most recently observed availability of a book asset.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AssetHealth {
    /// Lectern has not checked this asset since it was added or upgraded.
    #[default]
    Unknown,
    /// The asset was a readable regular file during the last scan.
    Available,
    /// The asset path did not exist during the last scan.
    Missing,
    /// The asset existed but could not be used as a readable regular file.
    Unreadable,
}

impl AssetHealth {
    /// Returns the stable lowercase storage value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Available => "available",
            Self::Missing => "missing",
            Self::Unreadable => "unreadable",
        }
    }

    /// Parses a stable storage value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "unknown" => Some(Self::Unknown),
            "available" => Some(Self::Available),
            "missing" => Some(Self::Missing),
            "unreadable" => Some(Self::Unreadable),
            _ => None,
        }
    }

    /// Returns whether the most recent check found a file problem.
    #[must_use]
    pub const fn has_issue(self) -> bool {
        matches!(self, Self::Missing | Self::Unreadable)
    }
}

impl fmt::Display for AssetHealth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown => formatter.write_str("Not checked"),
            Self::Available => formatter.write_str("Available"),
            Self::Missing => formatter.write_str("Missing"),
            Self::Unreadable => formatter.write_str("Unreadable"),
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
    /// Most recently observed availability of the file.
    pub health: AssetHealth,
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
    /// Sort by series identity and exact index, with books outside a series last.
    Series,
}

impl SortOrder {
    /// All sort orders exposed by the application.
    pub const ALL: [Self; 4] = [Self::Title, Self::Author, Self::RecentlyAdded, Self::Series];

    /// Returns the stable lowercase persistence value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Author => "author",
            Self::RecentlyAdded => "recently_added",
            Self::Series => "series",
        }
    }

    /// Parses a stable lowercase persistence value.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "title" => Some(Self::Title),
            "author" => Some(Self::Author),
            "recently_added" => Some(Self::RecentlyAdded),
            "series" => Some(Self::Series),
            _ => None,
        }
    }
}

impl fmt::Display for SortOrder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Title => formatter.write_str("Title"),
            Self::Author => formatter.write_str("Author"),
            Self::RecentlyAdded => formatter.write_str("Recently added"),
            Self::Series => formatter.write_str("Series"),
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
    /// Optional last-observed asset-health filter.
    pub asset_health: Option<AssetHealth>,
    /// Exact normalized entity facets combined conjunctively with search and asset filters.
    pub facets: organisation::ExactFacets,
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
    /// Optional exact series position.
    pub series_index: Option<organisation::SeriesIndex>,
    /// Whether a cached cover thumbnail is available.
    pub has_cover: bool,
    /// Whether any attached asset was last found missing or unreadable.
    pub has_file_issue: bool,
}

/// One bounded, ordered window of a library projection.
///
/// The complete result count remains available to virtualized presentations without requiring
/// them to allocate every matching [`BookSummary`]. `offset` is the zero-based result position of
/// the first returned summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LibraryPage {
    /// Number of logical books that match the projection.
    pub total: u64,
    /// Zero-based result position of the first returned summary.
    pub offset: u64,
    /// Ordered summaries in this bounded result window.
    pub books: Vec<BookSummary>,
}

/// Summary returned after checking the externally referenced assets in a library.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AssetHealthReport {
    /// Number of reference assets checked.
    pub checked: usize,
    /// Number of reference assets found as readable files.
    pub available: usize,
    /// Number of reference assets that no longer existed.
    pub missing: usize,
    /// Number of reference assets that existed but could not be read as files.
    pub unreadable: usize,
    /// Number of stored health states changed by the scan.
    pub changed: usize,
}

/// One publication that could not be imported.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportFailure {
    /// Source path that failed.
    pub path: PathBuf,
    /// Human-readable cause.
    pub message: String,
}

/// Monotonic progress emitted by a publication import workflow.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ImportProgress {
    /// Number of supported publication files found before parsing began.
    pub discovered: usize,
    /// Number of files parsed or rejected so far.
    pub processed: usize,
    /// Number of files committed to the library.
    pub imported: usize,
    /// Number of files that could not be parsed.
    pub failed: usize,
}

/// Final outcome of a completed publication import workflow.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ImportSummary {
    /// Number of supported publication files found.
    pub discovered: usize,
    /// Number of files committed to the library.
    pub imported: usize,
    /// Number of files that could not be parsed.
    pub failed: usize,
    /// Per-file parse failures.
    pub failures: Vec<ImportFailure>,
}

/// File-health observations made by a read-only library diagnostic.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReferencedFileDiagnostics {
    /// Number of externally referenced assets checked.
    pub checked: u64,
    /// Number of references that resolved to readable regular files.
    pub available: u64,
    /// Number of references whose paths did not exist.
    pub missing: u64,
    /// Number of references that existed but could not be read as regular files.
    pub unreadable: u64,
    /// Number of stored paths that could not be decoded.
    pub invalid_paths: u64,
    /// Number of observations that disagree with the last stored health state.
    pub stale_health: u64,
}

/// Read-only integrity and relationship checks for one library.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LibraryDiagnostics {
    /// Schema version recorded by the database.
    pub schema_version: i64,
    /// Newest schema version supported by this build.
    pub supported_schema_version: i64,
    /// Messages returned by `SQLite`'s physical integrity check; empty means healthy.
    pub sqlite_integrity_errors: Vec<String>,
    /// Number of rows returned by `SQLite`'s foreign-key check.
    pub foreign_key_violations: u64,
    /// Error returned by the FTS5 integrity check, if any.
    pub fts_error: Option<String>,
    /// Number of logical books without a file asset.
    pub books_without_assets: u64,
    /// Number of repeated `(book, format)` relationships.
    pub duplicate_book_formats: u64,
    /// Number of repeated externally referenced paths.
    pub duplicate_reference_paths: u64,
    /// Number of asset rows whose owner, format, storage mode, health, or path is invalid.
    pub invalid_asset_relationships: u64,
    /// Current observations for externally referenced files.
    pub referenced_files: ReferencedFileDiagnostics,
    /// Managed assets not checked until managed storage and hashes are implemented.
    pub unchecked_managed_assets: u64,
}

impl LibraryDiagnostics {
    /// Returns whether every implemented diagnostic check passed.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.schema_version == self.supported_schema_version
            && self.sqlite_integrity_errors.is_empty()
            && self.foreign_key_violations == 0
            && self.fts_error.is_none()
            && self.books_without_assets == 0
            && self.duplicate_book_formats == 0
            && self.duplicate_reference_paths == 0
            && self.invalid_asset_relationships == 0
            && self.referenced_files.missing == 0
            && self.referenced_files.unreadable == 0
            && self.referenced_files.invalid_paths == 0
            && self.referenced_files.stale_health == 0
    }
}

/// Compact operational counts for one library.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LibraryStats {
    /// Number of logical books.
    pub books: u64,
    /// Number of attached file assets.
    pub assets: u64,
    /// Number of cached book covers.
    pub covers: u64,
    /// Number of EPUB assets.
    pub epub_assets: u64,
    /// Number of PDF assets.
    pub pdf_assets: u64,
    /// Number of externally referenced assets.
    pub referenced_assets: u64,
    /// Number of Lectern-managed assets.
    pub managed_assets: u64,
    /// Number of assets not checked since import or migration.
    pub unknown_assets: u64,
    /// Number of assets last observed as available.
    pub available_assets: u64,
    /// Number of assets last observed as missing.
    pub missing_assets: u64,
    /// Number of assets last observed as unreadable.
    pub unreadable_assets: u64,
}

/// Outcome of a consistent library database snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupReport {
    /// Final backup path.
    pub destination: PathBuf,
    /// Snapshot size after durable publication.
    pub bytes: u64,
    /// Logical-book count captured in the snapshot.
    pub books: u64,
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
    /// Authoritative ordered normalized contributor credits.
    pub contributors: Vec<organisation::ContributorCredit>,
    /// Authoritative optional normalized series relation.
    pub series_membership: Option<organisation::SeriesMembership>,
    /// Authoritative normalized tags in display-name order.
    pub tags: Vec<organisation::Tag>,
    /// Fixed genres assigned to this canonical book, in catalog order.
    pub genres: Vec<organisation::Genre>,
    /// Virtual libraries containing this canonical book, in display-name order.
    pub virtual_libraries: Vec<organisation::VirtualLibrary>,
    /// Optional publisher.
    pub publisher: Option<String>,
    /// Optional calendar publication date.
    pub publication_date: Option<PublicationDate>,
    /// Optional publication language.
    pub language: Option<String>,
    /// Optional description or synopsis.
    pub description: Option<String>,
    /// Personal zero-to-five-star rating in exact half-star steps.
    pub rating: BookRating,
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
    /// Optional calendar publication date supplied by the source publication.
    pub publication_date: Option<PublicationDate>,
    /// Optional publication language.
    pub language: Option<String>,
    /// Optional description or synopsis.
    pub description: Option<String>,
    /// Publication-derived normalized organisation, when the adapter preserves source boundaries.
    pub imported_organisation: Option<organisation::ImportedOrganisation>,
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
    /// Optional calendar publication date supplied by the source publication.
    pub publication_date: Option<PublicationDate>,
    /// Optional publication language.
    pub language: Option<String>,
    /// Optional description or synopsis.
    pub description: Option<String>,
    /// Discovered ebook format.
    pub format: BookFormat,
    /// Original ebook path.
    pub source_path: PathBuf,
}

/// Logical book and assets ready for transactional import.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BookImport {
    /// Metadata shared by every file representation.
    pub book: BookMetadataDraft,
    /// One or more file representations to attach to the book.
    pub assets: Vec<BookAssetDraft>,
    /// Optional JPEG thumbnail bytes shared by the logical book.
    pub cover_thumbnail: Option<Vec<u8>>,
}

/// Single-publication compatibility input for transactional import.
///
/// New import adapters that know several files represent one book should use [`BookImport`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportRecord {
    /// Parsed metadata, format, and source path.
    pub book: BookDraft,
    /// Optional JPEG thumbnail bytes.
    pub cover_thumbnail: Option<Vec<u8>>,
}

/// Product policy applied when an import resolves to an already-known referenced path.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReimportMetadataPolicy {
    /// Keep user-edited metadata and refresh only assets and cover data.
    #[default]
    PreserveExisting,
    /// Replace stored metadata with the newly parsed publication metadata.
    ReplaceExisting,
}

/// Workflow-level application boundary used by Lectern frontends.
///
/// Implementations compose domain policy with persistence, publication parsing, and filesystem
/// adapters. Desktop and command-line frontends should depend on this boundary instead of invoking
/// a database adapter directly.
#[allow(clippy::missing_errors_doc)]
pub trait LibraryService {
    /// Failure returned by the composed workflow implementation.
    type Error: Error + Send + Sync + 'static;

    /// Returns all compact results matching a library projection.
    fn query_library(&mut self, query: &LibraryQuery) -> Result<Vec<BookSummary>, Self::Error>;

    /// Returns a bounded result page and its matching count.
    fn query_library_page(
        &mut self,
        query: &LibraryQuery,
        offset: u64,
        limit: u32,
    ) -> Result<LibraryPage, Self::Error>;

    /// Returns a bounded result window without recounting the projection.
    fn query_library_window(
        &mut self,
        query: &LibraryQuery,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<BookSummary>, Self::Error>;

    /// Captures the exact matching count and invalidation token for a query-backed selection.
    fn selection_snapshot(
        &mut self,
        query: &LibraryQuery,
    ) -> Result<organisation::SelectionSnapshot, Self::Error>;

    /// Returns only stable book identities for one ordered projection range.
    fn query_library_ids_window(
        &mut self,
        query: &LibraryQuery,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<BookId>, Self::Error>;

    /// Returns one bounded page of tag usage across a compact selection descriptor.
    fn selection_tag_usage(
        &mut self,
        selection: &organisation::BookSelection,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<organisation::SelectionTagUsage>, Self::Error>;

    /// Applies one atomic, set-based tag edit to a compact selection descriptor.
    fn apply_bulk_tags(
        &mut self,
        selection: &organisation::BookSelection,
        edit: &organisation::BulkTagEdit,
    ) -> Result<organisation::BulkTagResult, Self::Error>;

    /// Atomically removes a compact selection while leaving publication files untouched.
    fn remove_books(
        &mut self,
        selection: &organisation::BookSelection,
    ) -> Result<organisation::BulkRemovalResult, Self::Error>;

    /// Loads complete metadata and assets for one book.
    fn get_book(&mut self, id: BookId) -> Result<Option<Book>, Self::Error>;

    /// Persists normalized editable metadata without changing asset relationships.
    fn update_metadata(&mut self, edit: &organisation::BookEdit) -> Result<(), Self::Error>;

    /// Returns bounded contributor suggestions, prioritizing selected values.
    fn autocomplete_contributors(
        &mut self,
        prefix: &str,
        selected: &[organisation::ContributorId],
        limit: u32,
    ) -> Result<Vec<organisation::ContributorUsage>, Self::Error>;

    /// Returns bounded series suggestions, prioritizing selected values.
    fn autocomplete_series(
        &mut self,
        prefix: &str,
        selected: &[organisation::SeriesId],
        limit: u32,
    ) -> Result<Vec<organisation::SeriesUsage>, Self::Error>;

    /// Reports whether one exact book number is unused by the other books in a series.
    fn series_index_is_available(
        &mut self,
        series: organisation::SeriesId,
        index: organisation::SeriesIndex,
        excluding_book: BookId,
    ) -> Result<bool, Self::Error>;

    /// Returns bounded tag suggestions, prioritizing selected values.
    fn autocomplete_tags(
        &mut self,
        prefix: &str,
        selected: &[organisation::TagId],
        limit: u32,
    ) -> Result<Vec<organisation::TagUsage>, Self::Error>;

    /// Creates a virtual library, optionally assigning one book in the same transaction.
    fn create_virtual_library(
        &mut self,
        name: &str,
        description: Option<&str>,
        icon: organisation::VirtualLibraryIcon,
        book: Option<BookId>,
    ) -> Result<organisation::VirtualLibrary, Self::Error>;

    /// Returns bounded virtual-library suggestions, prioritizing selected values.
    fn autocomplete_virtual_libraries(
        &mut self,
        prefix: &str,
        selected: &[organisation::VirtualLibraryId],
        limit: u32,
    ) -> Result<Vec<organisation::VirtualLibrary>, Self::Error>;

    /// Adds or removes one canonical book from a virtual library atomically.
    fn set_book_virtual_library_membership(
        &mut self,
        book: BookId,
        library: organisation::VirtualLibraryId,
        included: bool,
    ) -> Result<organisation::VirtualLibraryMembershipResult, Self::Error>;

    /// Returns one bounded contributor vocabulary page with global usage counts.
    fn search_contributors(
        &mut self,
        prefix: &str,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<organisation::ContributorUsage>, Self::Error>;

    /// Returns one bounded series vocabulary page with global usage counts.
    fn search_series(
        &mut self,
        prefix: &str,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<organisation::SeriesUsage>, Self::Error>;

    /// Returns one bounded tag vocabulary page with global usage counts.
    fn search_tags(
        &mut self,
        prefix: &str,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<organisation::TagUsage>, Self::Error>;

    /// Lists durable saved projections alphabetically.
    fn list_saved_searches(&mut self) -> Result<Vec<organisation::SavedSearch>, Self::Error>;

    /// Returns one bounded saved-search manager page in normalized-name order.
    fn search_saved_searches(
        &mut self,
        prefix: &str,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<organisation::SavedSearch>, Self::Error>;

    /// Creates one named canonical projection and returns its stable identity.
    fn create_saved_search(
        &mut self,
        name: &str,
        query: &LibraryQuery,
    ) -> Result<organisation::SavedSearchId, Self::Error>;

    /// Explicitly replaces one saved projection without changing its name or identity.
    fn update_saved_search(
        &mut self,
        id: organisation::SavedSearchId,
        query: &LibraryQuery,
    ) -> Result<(), Self::Error>;

    /// Renames one saved projection without changing its query or identity.
    fn rename_saved_search(
        &mut self,
        id: organisation::SavedSearchId,
        name: &str,
    ) -> Result<(), Self::Error>;

    /// Deletes one saved projection without changing books or vocabulary.
    fn delete_saved_search(&mut self, id: organisation::SavedSearchId)
    -> Result<bool, Self::Error>;

    /// Counts book and saved-search references affected by a contributor mutation.
    fn contributor_mutation_impact(
        &mut self,
        id: organisation::ContributorId,
    ) -> Result<organisation::VocabularyMutationResult, Self::Error>;

    /// Counts book and saved-search references affected by a series mutation.
    fn series_mutation_impact(
        &mut self,
        id: organisation::SeriesId,
    ) -> Result<organisation::VocabularyMutationResult, Self::Error>;

    /// Counts book and saved-search references affected by a tag mutation.
    fn tag_mutation_impact(
        &mut self,
        id: organisation::TagId,
    ) -> Result<organisation::VocabularyMutationResult, Self::Error>;

    /// Renames a contributor and rebuilds affected projections atomically.
    fn rename_contributor(
        &mut self,
        id: organisation::ContributorId,
        display_name: &str,
        sort_name: &str,
    ) -> Result<organisation::VocabularyMutationResult, Self::Error>;

    /// Merges a contributor source into an explicit target atomically.
    fn merge_contributors(
        &mut self,
        source: organisation::ContributorId,
        target: organisation::ContributorId,
    ) -> Result<organisation::VocabularyMutationResult, Self::Error>;

    /// Deletes one unused contributor.
    fn delete_contributor(&mut self, id: organisation::ContributorId) -> Result<(), Self::Error>;

    /// Renames a series and rebuilds affected projections atomically.
    fn rename_series(
        &mut self,
        id: organisation::SeriesId,
        name: &str,
    ) -> Result<organisation::VocabularyMutationResult, Self::Error>;

    /// Merges a series source into an explicit target atomically.
    fn merge_series(
        &mut self,
        source: organisation::SeriesId,
        target: organisation::SeriesId,
    ) -> Result<organisation::VocabularyMutationResult, Self::Error>;

    /// Deletes one unused series.
    fn delete_series(&mut self, id: organisation::SeriesId) -> Result<(), Self::Error>;

    /// Renames a tag and rebuilds affected search projections atomically.
    fn rename_tag(
        &mut self,
        id: organisation::TagId,
        name: &str,
    ) -> Result<organisation::VocabularyMutationResult, Self::Error>;

    /// Merges a tag source into an explicit target atomically.
    fn merge_tags(
        &mut self,
        source: organisation::TagId,
        target: organisation::TagId,
    ) -> Result<organisation::VocabularyMutationResult, Self::Error>;

    /// Deletes a tag only when its current usage matches the confirmed impact.
    fn delete_tag(
        &mut self,
        id: organisation::TagId,
        confirmed: organisation::VocabularyMutationResult,
    ) -> Result<organisation::VocabularyMutationResult, Self::Error>;

    /// Discovers, parses, and imports publications using the application's merge policy.
    fn import_publications(
        &mut self,
        roots: &[PathBuf],
        report_progress: &mut dyn FnMut(ImportProgress),
    ) -> Result<ImportSummary, Self::Error>;

    /// Validates and attaches a referenced publication to a logical book.
    fn attach_asset(
        &mut self,
        book: BookId,
        format: BookFormat,
        path: &Path,
    ) -> Result<AssetId, Self::Error>;

    /// Detaches one non-final asset relationship.
    fn detach_asset(&mut self, asset: AssetId) -> Result<BookId, Self::Error>;

    /// Relinks an unavailable referenced asset after validating its replacement.
    fn relink_asset(
        &mut self,
        asset: AssetId,
        format: BookFormat,
        replacement_path: &Path,
    ) -> Result<(), Self::Error>;

    /// Deliberately replaces a referenced asset after validating its replacement.
    fn replace_asset(
        &mut self,
        asset: AssetId,
        format: BookFormat,
        replacement_path: &Path,
    ) -> Result<(), Self::Error>;

    /// Removes one logical book while leaving publication files untouched.
    fn remove_book(&mut self, id: BookId) -> Result<bool, Self::Error>;

    /// Rechecks externally referenced files and stores changed health observations.
    fn scan_assets(&mut self) -> Result<AssetHealthReport, Self::Error>;

    /// Creates a consistent database snapshot at a new destination.
    fn backup(&mut self, destination: &Path) -> Result<BackupReport, Self::Error>;

    /// Runs read-only integrity, relationship, index, and referenced-file checks.
    fn doctor(&mut self) -> Result<LibraryDiagnostics, Self::Error>;

    /// Returns compact library and asset counts.
    fn stats(&mut self) -> Result<LibraryStats, Self::Error>;

    /// Loads a cached JPEG cover thumbnail.
    fn load_cover(&mut self, id: BookId) -> Result<Option<Vec<u8>>, Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::{
        AssetHealth, AssetId, AssetStorage, BookFormat, BookId, BookRating, BuildInfo,
        PublicationDate,
    };

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

    #[test]
    fn asset_health_has_stable_values_and_issue_states() {
        assert_eq!(AssetHealth::Unknown.as_str(), "unknown");
        assert_eq!(AssetHealth::Available.as_str(), "available");
        assert_eq!(AssetHealth::Missing.as_str(), "missing");
        assert_eq!(AssetHealth::Unreadable.as_str(), "unreadable");
        assert_eq!(AssetHealth::parse("missing"), Some(AssetHealth::Missing));
        assert_eq!(AssetHealth::parse("offline"), None);
        assert!(!AssetHealth::Unknown.has_issue());
        assert!(!AssetHealth::Available.has_issue());
        assert!(AssetHealth::Missing.has_issue());
        assert!(AssetHealth::Unreadable.has_issue());
    }

    #[test]
    fn publication_dates_are_exact_partial_or_complete_calendar_dates() {
        for (input, year, month, day) in [
            ("1969", 1969, None, None),
            ("2000-02", 2000, Some(2), None),
            ("2024-02-29", 2024, Some(2), Some(29)),
        ] {
            let date = input.parse::<PublicationDate>().expect("valid date");
            assert_eq!(date.year(), year);
            assert_eq!(date.month(), month);
            assert_eq!(date.day(), day);
            assert_eq!(date.to_string(), input);
        }

        for invalid in [
            "",
            "0000",
            "24",
            "2024-00",
            "2024-13",
            "2023-02-29",
            "2024-04-31",
            "2024-1-01",
            "2024/01/01",
            "2024-01-01T00:00:00Z",
        ] {
            assert!(
                invalid.parse::<PublicationDate>().is_err(),
                "{invalid:?} should be invalid"
            );
        }
    }

    #[test]
    fn ratings_are_exact_and_bounded_to_half_stars() {
        for (half_stars, display) in [(0, "0"), (1, "0.5"), (7, "3.5"), (10, "5")] {
            let rating = BookRating::from_half_stars(half_stars).expect("valid rating");
            assert_eq!(rating.half_stars(), half_stars);
            assert_eq!(rating.to_string(), display);
        }
        assert_eq!(BookRating::default().half_stars(), 0);
        assert_eq!(BookRating::from_half_stars(11), None);
    }
}
