//! `SQLite` persistence adapter for Lectern.

mod organisation;

use std::{
    cell::Cell,
    ffi::OsString,
    fs::{File, OpenOptions},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(windows)]
use std::os::windows::ffi::{OsStrExt, OsStringExt};

use lectern_core::organisation::{
    BookEdit, BookSelection, BulkRemovalResult, BulkTagEdit, BulkTagResult, ContributorId,
    ContributorUsage, IdentifierTypeUsage, LibraryGeneration, NameKind, SavedSearch, SavedSearchId,
    SearchClause, SearchExpression, SearchParseError, SelectionSnapshot, SelectionTagUsage,
    SeriesId, SeriesIndex, SeriesUsage, TagColor, TagId, TagReference, TagUsage, TextMatch,
    VocabularyMutationResult, identity_key, normalize_name,
};
use lectern_core::{
    AssetHealth, AssetHealthReport, AssetId, AssetStorage, BackupReport, Book, BookAsset,
    BookAssetDraft, BookDraft, BookFormat, BookId, BookImport, BookMetadataDraft, BookSummary,
    ImportRecord, LibraryDiagnostics, LibraryPage, LibraryQuery, LibraryStats,
    ReferencedFileDiagnostics, ReimportMetadataPolicy, SortOrder,
};
use rayon::prelude::*;
use rusqlite::{
    Connection, OptionalExtension, Statement, Transaction, TransactionBehavior, params,
};
use thiserror::Error;

#[cfg(not(any(unix, windows)))]
compile_error!("Lectern's lossless path codec currently supports Unix and Windows targets");

const SCHEMA_VERSION: i64 = 9;
const BACKUP_PAGES_PER_STEP: i32 = 2_048;
static NEXT_BACKUP_TEMP_ID: AtomicU64 = AtomicU64::new(0);

const SCHEMA: &str = r"
CREATE TABLE books (
    id           INTEGER PRIMARY KEY,
    title        TEXT NOT NULL,
    sort_title   TEXT NOT NULL,
    authors      TEXT NOT NULL,
    sort_authors TEXT NOT NULL,
    series       TEXT,
    publisher    TEXT,
    language     TEXT,
    description  TEXT,
    has_cover    INTEGER NOT NULL DEFAULT 0 CHECK (has_cover IN (0, 1)),
    has_file_issue INTEGER NOT NULL DEFAULT 0 CHECK (has_file_issue IN (0, 1)),
    added_at     INTEGER NOT NULL DEFAULT (unixepoch()),
    modified_at  INTEGER NOT NULL DEFAULT (unixepoch())
) STRICT;

CREATE INDEX books_sort_title_idx ON books(sort_title, id);
CREATE INDEX books_sort_authors_idx ON books(sort_authors, sort_title, id);
CREATE INDEX books_added_at_idx ON books(added_at DESC, id DESC);

CREATE TABLE book_assets (
    id            INTEGER PRIMARY KEY,
    book_id       INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    format        TEXT NOT NULL CHECK (
        length(format) BETWEEN 1 AND 32
        AND format = lower(format)
        AND format NOT GLOB '*[^a-z0-9.+_-]*'
    ),
    storage_mode  TEXT NOT NULL DEFAULT 'reference'
                  CHECK (storage_mode IN ('reference', 'managed')),
    health        TEXT NOT NULL DEFAULT 'unknown'
                  CHECK (health IN ('unknown', 'available', 'missing', 'unreadable')),
    path_encoding TEXT NOT NULL DEFAULT 'utf8'
                  CHECK (path_encoding IN ('utf8', 'unix', 'windows')),
    path          BLOB NOT NULL CHECK (length(path) > 0),
    added_at      INTEGER NOT NULL DEFAULT (unixepoch()),
    modified_at   INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE(book_id, format),
    CHECK (storage_mode <> 'managed' OR path_encoding = 'utf8'),
    CHECK (path_encoding <> 'windows' OR length(path) % 2 = 0)
) STRICT;

CREATE UNIQUE INDEX book_assets_reference_path_uidx
    ON book_assets(path_encoding, path)
    WHERE storage_mode = 'reference';
CREATE INDEX book_assets_format_book_idx ON book_assets(format, book_id);
CREATE INDEX book_assets_health_book_idx ON book_assets(health, book_id);

CREATE TABLE book_covers (
    book_id INTEGER PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
    jpeg    BLOB NOT NULL
) STRICT;

CREATE TRIGGER book_covers_after_insert_summary
AFTER INSERT ON book_covers BEGIN
    UPDATE books SET has_cover = 1 WHERE id = new.book_id;
END;

CREATE TRIGGER book_covers_after_delete_summary
AFTER DELETE ON book_covers BEGIN
    UPDATE books SET has_cover = 0 WHERE id = old.book_id;
END;

CREATE TRIGGER book_assets_after_insert_summary
AFTER INSERT ON book_assets
WHEN new.health IN ('missing', 'unreadable')
BEGIN
    UPDATE books SET has_file_issue = 1 WHERE id = new.book_id;
END;

CREATE TRIGGER book_assets_after_health_update_summary
AFTER UPDATE OF health ON book_assets
WHEN old.health IS NOT new.health
BEGIN
    UPDATE books
    SET has_file_issue = EXISTS(
        SELECT 1 FROM book_assets
        WHERE book_id = new.book_id AND health IN ('missing', 'unreadable')
    )
    WHERE id = new.book_id;
END;

CREATE TRIGGER book_assets_after_delete_summary
AFTER DELETE ON book_assets
BEGIN
    UPDATE books
    SET has_file_issue = EXISTS(
        SELECT 1 FROM book_assets
        WHERE book_id = old.book_id AND health IN ('missing', 'unreadable')
    )
    WHERE id = old.book_id;
END;

CREATE VIRTUAL TABLE books_fts USING fts5(
    title,
    authors,
    series,
    publisher,
    content='books',
    content_rowid='id',
    tokenize='unicode61 remove_diacritics 2',
    prefix='2 3'
);

CREATE TRIGGER books_after_insert AFTER INSERT ON books BEGIN
    INSERT INTO books_fts(rowid, title, authors, series, publisher)
    VALUES (new.id, new.title, new.authors, new.series, new.publisher);
END;

CREATE TRIGGER books_after_delete AFTER DELETE ON books BEGIN
    INSERT INTO books_fts(books_fts, rowid, title, authors, series, publisher)
    VALUES ('delete', old.id, old.title, old.authors, old.series, old.publisher);
END;

CREATE TRIGGER books_after_update
AFTER UPDATE OF title, authors, series, publisher ON books
WHEN old.title IS NOT new.title
  OR old.authors IS NOT new.authors
  OR old.series IS NOT new.series
  OR old.publisher IS NOT new.publisher
BEGIN
    INSERT INTO books_fts(books_fts, rowid, title, authors, series, publisher)
    VALUES ('delete', old.id, old.title, old.authors, old.series, old.publisher);
    INSERT INTO books_fts(rowid, title, authors, series, publisher)
    VALUES (new.id, new.title, new.authors, new.series, new.publisher);
END;
";

const MIGRATE_1_OR_2_TO_3: &str = r"
DROP TRIGGER books_after_insert;
DROP TRIGGER books_after_delete;
DROP TRIGGER books_after_update;
DROP TABLE books_fts;

CREATE TABLE books_new (
    id           INTEGER PRIMARY KEY,
    title        TEXT NOT NULL,
    sort_title   TEXT NOT NULL,
    authors      TEXT NOT NULL,
    sort_authors TEXT NOT NULL,
    series       TEXT,
    publisher    TEXT,
    language     TEXT,
    description  TEXT,
    added_at     INTEGER NOT NULL DEFAULT (unixepoch()),
    modified_at  INTEGER NOT NULL DEFAULT (unixepoch())
) STRICT;

INSERT INTO books_new (
    id, title, sort_title, authors, sort_authors, series,
    publisher, language, description, added_at, modified_at
)
SELECT
    id, title, sort_title, authors, sort_authors, series,
    publisher, language, description, added_at, modified_at
FROM books;

CREATE TABLE book_assets (
    id            INTEGER PRIMARY KEY,
    book_id       INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
    format        TEXT NOT NULL CHECK (
        length(format) BETWEEN 1 AND 32
        AND format = lower(format)
        AND format NOT GLOB '*[^a-z0-9.+_-]*'
    ),
    storage_mode  TEXT NOT NULL DEFAULT 'reference'
                  CHECK (storage_mode IN ('reference', 'managed')),
    path_encoding TEXT NOT NULL DEFAULT 'utf8'
                  CHECK (path_encoding IN ('utf8', 'unix', 'windows')),
    path          BLOB NOT NULL CHECK (length(path) > 0),
    added_at      INTEGER NOT NULL DEFAULT (unixepoch()),
    modified_at   INTEGER NOT NULL DEFAULT (unixepoch()),
    UNIQUE(book_id, format),
    CHECK (storage_mode <> 'managed' OR path_encoding = 'utf8'),
    CHECK (path_encoding <> 'windows' OR length(path) % 2 = 0)
) STRICT;

INSERT INTO book_assets (
    id, book_id, format, storage_mode, path_encoding, path, added_at, modified_at
)
SELECT
    id, id, format, 'reference', 'utf8', CAST(source_path AS BLOB), added_at, modified_at
FROM books
ORDER BY id;

DROP TABLE books;
ALTER TABLE books_new RENAME TO books;

CREATE INDEX books_sort_title_idx ON books(sort_title, id);
CREATE INDEX books_sort_authors_idx ON books(sort_authors, sort_title, id);
CREATE INDEX books_added_at_idx ON books(added_at DESC, id DESC);

CREATE UNIQUE INDEX book_assets_reference_path_uidx
    ON book_assets(path_encoding, path)
    WHERE storage_mode = 'reference';
CREATE INDEX book_assets_format_book_idx ON book_assets(format, book_id);

CREATE VIRTUAL TABLE books_fts USING fts5(
    title,
    authors,
    series,
    publisher,
    content='books',
    content_rowid='id',
    tokenize='unicode61 remove_diacritics 2',
    prefix='2 3'
);

CREATE TRIGGER books_after_insert AFTER INSERT ON books BEGIN
    INSERT INTO books_fts(rowid, title, authors, series, publisher)
    VALUES (new.id, new.title, new.authors, new.series, new.publisher);
END;

CREATE TRIGGER books_after_delete AFTER DELETE ON books BEGIN
    INSERT INTO books_fts(books_fts, rowid, title, authors, series, publisher)
    VALUES ('delete', old.id, old.title, old.authors, old.series, old.publisher);
END;

CREATE TRIGGER books_after_update
AFTER UPDATE OF title, authors, series, publisher ON books
WHEN old.title IS NOT new.title
  OR old.authors IS NOT new.authors
  OR old.series IS NOT new.series
  OR old.publisher IS NOT new.publisher
BEGIN
    INSERT INTO books_fts(books_fts, rowid, title, authors, series, publisher)
    VALUES ('delete', old.id, old.title, old.authors, old.series, old.publisher);
    INSERT INTO books_fts(rowid, title, authors, series, publisher)
    VALUES (new.id, new.title, new.authors, new.series, new.publisher);
END;

INSERT INTO books_fts(books_fts) VALUES ('rebuild');
";

const MIGRATE_3_TO_4: &str = r"
ALTER TABLE book_assets ADD COLUMN health TEXT NOT NULL DEFAULT 'unknown'
    CHECK (health IN ('unknown', 'available', 'missing', 'unreadable'));
CREATE INDEX book_assets_health_book_idx ON book_assets(health, book_id);
";

const MIGRATE_4_TO_5: &str = r"
ALTER TABLE books ADD COLUMN has_cover INTEGER NOT NULL DEFAULT 0
    CHECK (has_cover IN (0, 1));
ALTER TABLE books ADD COLUMN has_file_issue INTEGER NOT NULL DEFAULT 0
    CHECK (has_file_issue IN (0, 1));

UPDATE books
SET has_cover = EXISTS(
    SELECT 1 FROM book_covers WHERE book_id = books.id
);
UPDATE books
SET has_file_issue = EXISTS(
    SELECT 1 FROM book_assets
    WHERE book_id = books.id AND health IN ('missing', 'unreadable')
);

CREATE TRIGGER book_covers_after_insert_summary
AFTER INSERT ON book_covers BEGIN
    UPDATE books SET has_cover = 1 WHERE id = new.book_id;
END;

CREATE TRIGGER book_covers_after_delete_summary
AFTER DELETE ON book_covers BEGIN
    UPDATE books SET has_cover = 0 WHERE id = old.book_id;
END;

CREATE TRIGGER book_assets_after_insert_summary
AFTER INSERT ON book_assets
WHEN new.health IN ('missing', 'unreadable')
BEGIN
    UPDATE books SET has_file_issue = 1 WHERE id = new.book_id;
END;

CREATE TRIGGER book_assets_after_health_update_summary
AFTER UPDATE OF health ON book_assets
WHEN old.health IS NOT new.health
BEGIN
    UPDATE books
    SET has_file_issue = EXISTS(
        SELECT 1 FROM book_assets
        WHERE book_id = new.book_id AND health IN ('missing', 'unreadable')
    )
    WHERE id = new.book_id;
END;

CREATE TRIGGER book_assets_after_delete_summary
AFTER DELETE ON book_assets
BEGIN
    UPDATE books
    SET has_file_issue = EXISTS(
        SELECT 1 FROM book_assets
        WHERE book_id = old.book_id AND health IN ('missing', 'unreadable')
    )
    WHERE id = old.book_id;
END;
";

/// Failure returned by the persistence adapter.
#[derive(Debug, Error)]
pub enum StorageError {
    /// The underlying database operation failed.
    #[error("database operation failed: {0}")]
    Database(#[from] rusqlite::Error),
    /// A required filesystem operation failed.
    #[error("filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    /// The library was created by a newer, unsupported schema.
    #[error("unsupported library schema version {0}")]
    UnsupportedSchema(i64),
    /// The requested book no longer exists.
    #[error("book {0} was not found")]
    BookNotFound(BookId),
    /// The requested file asset no longer exists.
    #[error("asset {0} was not found")]
    AssetNotFound(AssetId),
    /// Detaching the selected asset would leave its logical book without a file.
    #[error("asset {asset} is the last file for book {book} and cannot be detached")]
    LastAssetDetach {
        /// Asset whose relationship must be retained.
        asset: AssetId,
        /// Logical book that would otherwise have no assets.
        book: BookId,
    },
    /// The selected logical book already has a file in the requested format.
    #[error("book {book} already has a {format} asset")]
    BookAlreadyHasFormat {
        /// Logical book that already owns the format.
        book: BookId,
        /// Format that cannot be attached twice.
        format: BookFormat,
    },
    /// An operation that requires a referenced asset was requested for a managed asset.
    #[error("asset {0} is managed and cannot be relinked as an external file")]
    AssetNotReference(AssetId),
    /// Relink recovery was requested even though the current reference remains readable.
    #[error("asset {0} is still available; use Replace file for a deliberate change")]
    RelinkAssetAvailable(AssetId),
    /// The replacement file did not match the asset's stored format.
    #[error("replacement format {found} does not match the expected {expected}")]
    RelinkFormatMismatch {
        /// Format stored for the asset being relinked.
        expected: BookFormat,
        /// Format validated from the replacement file.
        found: BookFormat,
    },
    /// A deliberate replacement did not match the asset's stored format.
    #[error("replacement format {found} does not match the expected {expected}")]
    ReplacementFormatMismatch {
        /// Format stored for the asset being replaced.
        expected: BookFormat,
        /// Format validated from the replacement file.
        found: BookFormat,
    },
    /// The proposed path was already linked by another reference asset.
    #[error("reference path is already linked by asset {0}")]
    ReferencePathInUse(AssetId),
    /// The file proposed for attachment was not usable when the transaction began.
    #[error("file to attach is {0}")]
    AttachmentUnavailable(AssetHealth),
    /// The proposed replacement file was not usable at the time of relinking.
    #[error("replacement file is {0}")]
    ReplacementUnavailable(AssetHealth),
    /// The database returned an impossible negative row count.
    #[error("database returned invalid book count {0}")]
    InvalidCount(i64),
    /// A requested library page began beyond `SQLite`'s signed integer range.
    #[error("library page offset {0} exceeds SQLite's supported range")]
    InvalidPageOffset(u64),
    /// A query-backed selection was created against an older library state.
    #[error("the selected projection changed; review the current match count before applying")]
    StaleSelection,
    /// A logical import contained no file representation.
    #[error("a logical book must contain at least one asset")]
    EmptyAssets,
    /// A logical import contained more than one file for a single format.
    #[error("a logical book cannot contain more than one {0} asset")]
    DuplicateAssetFormat(BookFormat),
    /// A logical import contained the same reference location more than once.
    #[error("a logical book cannot contain the same reference path more than once")]
    DuplicateAssetPath,
    /// Existing paths in one logical import belong to different books.
    #[error("incoming assets belong to different existing books")]
    ConflictingAssetOwners,
    /// An incoming asset used an empty or unsafe managed path.
    #[error("invalid asset path: {0}")]
    InvalidAssetPath(String),
    /// A stored asset format is not supported by this build.
    #[error("unsupported stored asset format '{0}'")]
    InvalidAssetFormat(String),
    /// A stored asset ownership mode is not supported by this build.
    #[error("unsupported stored asset storage mode '{0}'")]
    InvalidAssetStorage(String),
    /// A stored asset health value is not supported by this build.
    #[error("unsupported stored asset health '{0}'")]
    InvalidAssetHealth(String),
    /// A stored path uses an unknown platform encoding.
    #[error("unsupported stored path encoding '{0}'")]
    InvalidPathEncoding(String),
    /// Stored path bytes cannot be decoded using their declared encoding.
    #[error("invalid stored path bytes for '{0}' encoding")]
    InvalidPathData(String),
    /// A schema operation produced an invalid library.
    #[error("library integrity check failed: {0}")]
    Integrity(String),
    /// User-entered or imported curation metadata violated the shared domain contract.
    #[error("invalid curation metadata: {0}")]
    InvalidCuration(String),
    /// A structured library search was invalid and must not be dispatched.
    #[error("invalid structured search: {0}")]
    InvalidSearch(#[from] SearchParseError),
    /// A backup refused to replace an existing destination.
    #[error("backup destination already exists: {0}")]
    BackupDestinationExists(PathBuf),
    /// A backup destination did not name a file.
    #[error("backup destination must name a file: {0}")]
    InvalidBackupDestination(PathBuf),
}

/// Result type returned by storage operations.
pub type Result<T> = std::result::Result<T, StorageError>;

#[derive(Clone, Copy)]
enum ReferencePathUpdate {
    Relink,
    Replace,
}

/// Connection-scoped access to a Lectern library database.
pub struct LibraryDatabase {
    connection: Connection,
    logical_generation: Cell<u64>,
}

impl LibraryDatabase {
    /// Opens or creates a library at `path` and applies pending migrations.
    ///
    /// # Errors
    ///
    /// Returns an error when the parent directory cannot be created, the database cannot be
    /// opened or configured, or its schema is newer than this application supports.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        let connection = Connection::open(path)?;
        Self::from_connection(connection, true)
    }

    /// Creates an isolated in-memory library, primarily for tests and previews.
    ///
    /// # Errors
    ///
    /// Returns an error when the in-memory database cannot be initialized or migrated.
    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?, false)
    }

    fn from_connection(mut connection: Connection, persistent: bool) -> Result<Self> {
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        if persistent {
            configure_persistent_database(&connection)?;
        }

        initialize_schema(&mut connection)?;
        Ok(Self {
            connection,
            logical_generation: Cell::new(0),
        })
    }

    fn bump_generation(&self) {
        self.logical_generation
            .set(self.logical_generation.get().wrapping_add(1));
    }

    /// Returns the number of logical books in the library.
    ///
    /// # Errors
    ///
    /// Returns an error when the count query fails or returns an invalid value.
    pub fn count(&self) -> Result<u64> {
        let count: i64 = self
            .connection
            .prepare_cached("SELECT count(*) FROM books")?
            .query_row([], |row| row.get(0))?;
        u64::try_from(count).map_err(|_| StorageError::InvalidCount(count))
    }

    /// Inserts aggregate books or refreshes assets for already-known reference paths in one
    /// transaction.
    ///
    /// Metadata belongs to the logical book after its first import. A later automatic import that
    /// resolves to that book preserves its metadata so file contents cannot overwrite user edits.
    ///
    /// # Errors
    ///
    /// Returns an error when an aggregate is invalid, its existing paths resolve to different
    /// books, or the transaction cannot be committed.
    pub fn import_books(&mut self, records: &[BookImport]) -> Result<Vec<BookId>> {
        self.import_books_with_policy(records, ReimportMetadataPolicy::PreserveExisting)
    }

    /// Inserts aggregate books using an application-selected metadata merge policy.
    ///
    /// # Errors
    ///
    /// Returns an error when an aggregate is invalid, its existing paths resolve to different
    /// books, or the transaction cannot be committed.
    pub fn import_books_with_policy(
        &mut self,
        records: &[BookImport],
        metadata_policy: ReimportMetadataPolicy,
    ) -> Result<Vec<BookId>> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = {
            let mut statements = ImportStatements::prepare(&transaction)?;
            records
                .iter()
                .map(|record| {
                    upsert_book(
                        &transaction,
                        &mut statements,
                        MetadataInput::from(&record.book),
                        record.assets.iter().map(AssetInput::from),
                        record.cover_thumbnail.as_deref(),
                        metadata_policy,
                    )
                })
                .collect::<Result<Vec<_>>>()?
        };
        transaction.commit()?;
        self.bump_generation();
        Ok(ids)
    }

    /// Inserts independent publication files or refreshes assets for already-known reference
    /// paths in one transaction.
    ///
    /// This compatibility surface treats every record as a one-asset logical book. Aggregate
    /// importers should use [`Self::import_books`].
    ///
    /// # Errors
    ///
    /// Returns an error when the transaction cannot be started, a record cannot be written, or
    /// the transaction cannot be committed.
    pub fn import_batch(&mut self, records: &[ImportRecord]) -> Result<Vec<BookId>> {
        self.import_batch_with_policy(records, ReimportMetadataPolicy::PreserveExisting)
    }

    /// Imports compatibility records using an application-selected metadata merge policy.
    ///
    /// # Errors
    ///
    /// Returns an error when the transaction cannot be started, a record cannot be written, or
    /// the transaction cannot be committed.
    pub fn import_batch_with_policy(
        &mut self,
        records: &[ImportRecord],
        metadata_policy: ReimportMetadataPolicy,
    ) -> Result<Vec<BookId>> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = {
            let mut statements = ImportStatements::prepare(&transaction)?;
            records
                .iter()
                .map(|record| {
                    let asset = AssetInput {
                        format: record.book.format,
                        storage: AssetStorage::Reference,
                        path: &record.book.source_path,
                    };
                    upsert_book(
                        &transaction,
                        &mut statements,
                        MetadataInput::from(&record.book),
                        std::iter::once(asset),
                        record.cover_thumbnail.as_deref(),
                        metadata_policy,
                    )
                })
                .collect::<Result<Vec<_>>>()?
        };
        transaction.commit()?;
        self.bump_generation();
        Ok(ids)
    }

    /// Returns compact logical-book results matching `query`.
    ///
    /// # Errors
    ///
    /// Returns an error when the indexed query cannot be prepared or executed.
    pub fn query(&self, query: &LibraryQuery) -> Result<Vec<BookSummary>> {
        let plan = LibraryQueryPlan::new(query)?;
        let sql = format!(
            "SELECT b.id, b.title, b.authors, b.series, b.series_index, \
             b.has_cover, b.has_file_issue \
             FROM books b {} {where_clause} ORDER BY {order}",
            plan.joins,
            where_clause = plan.where_clause,
            order = plan.order,
        );

        let mut statement = self.connection.prepare_cached(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(plan.bindings), book_summary)?;

        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Returns one bounded, ordered window of the logical books matching `query`.
    ///
    /// The total count and summaries are read inside the same deferred transaction so a library
    /// mutation in another connection cannot produce a mismatched grid size and page. The caller
    /// supplies a zero-based `offset` and a bounded `limit`; a zero limit returns the total with
    /// no summaries.
    ///
    /// # Errors
    ///
    /// Returns an error when the offset cannot be represented by `SQLite` or either query cannot be
    /// prepared or executed.
    pub fn query_page(
        &mut self,
        query: &LibraryQuery,
        offset: u64,
        limit: u32,
    ) -> Result<LibraryPage> {
        let page_offset = offset;
        let offset = i64::try_from(offset).map_err(|_| StorageError::InvalidPageOffset(offset))?;
        let plan = LibraryQueryPlan::new(query)?;
        let transaction = self.connection.transaction()?;

        if offset == 0 && limit > 0 && plan.has_full_text_search {
            let (total, books) = query_first_page_with_total(&transaction, &plan, limit)?;
            transaction.commit()?;
            return Ok(LibraryPage {
                total,
                offset: page_offset,
                books,
            });
        }

        let total = {
            let count_sql = format!(
                "SELECT count(*) FROM books b {} {}",
                plan.joins, plan.where_clause
            );
            let mut statement = transaction.prepare_cached(&count_sql)?;
            let count: i64 = statement
                .query_row(rusqlite::params_from_iter(plan.bindings.iter()), |row| {
                    row.get(0)
                })?;
            u64::try_from(count).map_err(|_| StorageError::InvalidCount(count))?
        };

        let books = query_window_with_plan(&transaction, &plan, offset, limit)?;

        transaction.commit()?;
        Ok(LibraryPage {
            total,
            offset: page_offset,
            books,
        })
    }

    /// Returns a bounded ordered window of a library projection without recounting its matches.
    ///
    /// Use [`Self::query_page`] to establish a projection's total before requesting subsequent
    /// windows. This keeps scrolling proportional to the displayed data instead of repeating a
    /// full count for every page.
    ///
    /// # Errors
    ///
    /// Returns an error when the offset cannot be represented by `SQLite` or the indexed query
    /// cannot be prepared or executed.
    pub fn query_window(
        &self,
        query: &LibraryQuery,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<BookSummary>> {
        let offset = i64::try_from(offset).map_err(|_| StorageError::InvalidPageOffset(offset))?;
        let plan = LibraryQueryPlan::new(query)?;
        query_window_with_plan(&self.connection, &plan, offset, limit)
    }

    /// Returns the matching count and invalidation generation without loading summaries.
    ///
    /// # Errors
    ///
    /// Returns an error when the structured query or count cannot be evaluated.
    pub fn selection_snapshot(&mut self, query: &LibraryQuery) -> Result<SelectionSnapshot> {
        let plan = LibraryQueryPlan::new(query)?;
        let transaction = self.connection.transaction()?;
        let generation = current_generation(&transaction, self.logical_generation.get())?;
        let sql = format!(
            "SELECT count(*) FROM books b {} {}",
            plan.joins, plan.where_clause
        );
        let count: i64 = transaction
            .prepare_cached(&sql)?
            .query_row(rusqlite::params_from_iter(plan.bindings), |row| row.get(0))?;
        transaction.commit()?;
        Ok(SelectionSnapshot {
            matching_books: checked_count(count)?,
            generation,
        })
    }

    /// Returns only stable IDs for one ordered range in the current projection.
    ///
    /// # Errors
    ///
    /// Returns an error when the query is invalid or the offset exceeds `SQLite`'s range.
    pub fn query_ids_window(
        &self,
        query: &LibraryQuery,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<BookId>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let offset = i64::try_from(offset).map_err(|_| StorageError::InvalidPageOffset(offset))?;
        let plan = LibraryQueryPlan::new(query)?;
        let sql = format!(
            "SELECT b.id FROM books b {} {} ORDER BY {} LIMIT ?{} OFFSET ?{}",
            plan.joins,
            plan.where_clause,
            plan.order,
            plan.bindings.len() + 1,
            plan.bindings.len() + 2,
        );
        let mut bindings = plan.bindings;
        bindings.push(i64::from(limit).into());
        bindings.push(offset.into());
        let mut statement = self.connection.prepare_cached(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(bindings), |row| {
            Ok(BookId::new(row.get(0)?))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Returns bounded tag usage across a compact target descriptor.
    ///
    /// # Errors
    ///
    /// Returns an error when an all-matching descriptor is stale or its query cannot be resolved.
    pub fn selection_tag_usage(
        &mut self,
        selection: &BookSelection,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<SelectionTagUsage>> {
        let offset = i64::try_from(offset).map_err(|_| StorageError::InvalidPageOffset(offset))?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        prepare_selection(&transaction, selection, self.logical_generation.get())?;
        let mut statement = transaction.prepare_cached(
            "SELECT t.id, t.name, t.color, \
                    (SELECT count(*) FROM book_tags all_tags WHERE all_tags.tag_id = t.id), \
                    (SELECT count(*) FROM saved_search_included_tags si \
                     WHERE si.tag_id = t.id) + \
                    (SELECT count(*) FROM saved_search_excluded_tags se \
                     WHERE se.tag_id = t.id), \
                    (SELECT count(*) FROM book_tags selected_tags \
                     JOIN temp.lectern_selected_books selected \
                       ON selected.book_id = selected_tags.book_id \
                     WHERE selected_tags.tag_id = t.id) \
             FROM tags t \
             WHERE EXISTS (SELECT 1 FROM book_tags bt \
                           JOIN temp.lectern_selected_books selected \
                             ON selected.book_id = bt.book_id \
                           WHERE bt.tag_id = t.id) \
             ORDER BY t.identity_key, t.id LIMIT ?1 OFFSET ?2",
        )?;
        let rows = statement.query_map(params![i64::from(limit.min(100)), offset], |row| {
            let color_value = row.get::<_, String>(2)?;
            let color = TagColor::parse(&color_value).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    format!("invalid tag color {color_value:?}").into(),
                )
            })?;
            let books = row.get::<_, i64>(3)?;
            let saved = row.get::<_, i64>(4)?;
            let selected = row.get::<_, i64>(5)?;
            Ok((
                TagId::new(row.get(0)?),
                row.get::<_, String>(1)?,
                color,
                books,
                saved,
                selected,
            ))
        })?;
        let mut result = Vec::new();
        for row in rows {
            let (id, name, color, books, saved, selected) = row?;
            result.push(SelectionTagUsage {
                usage: TagUsage {
                    tag: lectern_core::organisation::Tag { id, name, color },
                    books: checked_count(books)?,
                    saved_searches: checked_count(saved)?,
                },
                selected_books: checked_count(selected)?,
            });
        }
        drop(statement);
        transaction.commit()?;
        Ok(result)
    }

    /// Applies disjoint tag additions and removals to a compact target in one durable transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when the target is stale, a tag is invalid or absent, add/remove sets
    /// overlap, or any relationship/projection/FTS update cannot commit.
    pub fn apply_bulk_tags(
        &mut self,
        selection: &BookSelection,
        edit: &BulkTagEdit,
    ) -> Result<BulkTagResult> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (books_matched, _) =
            prepare_selection(&transaction, selection, self.logical_generation.get())?;
        prepare_bulk_tag_tables(&transaction)?;

        let mut tags_created = 0_u64;
        for reference in &edit.add {
            let (id, created) = resolve_bulk_tag(&transaction, reference)?;
            tags_created += u64::from(created);
            transaction.execute(
                "INSERT OR IGNORE INTO temp.lectern_bulk_add_tags(tag_id) VALUES (?1)",
                [id.value()],
            )?;
        }
        let mut remove = edit.remove.clone();
        remove.sort_unstable();
        remove.dedup();
        for id in remove {
            let exists = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM tags WHERE id = ?1)",
                [id.value()],
                |row| row.get::<_, bool>(0),
            )?;
            if !exists {
                return Err(StorageError::InvalidCuration(format!(
                    "tag {id} does not exist"
                )));
            }
            let overlaps = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM temp.lectern_bulk_add_tags WHERE tag_id = ?1)",
                [id.value()],
                |row| row.get::<_, bool>(0),
            )?;
            if overlaps {
                return Err(StorageError::InvalidCuration(format!(
                    "tag {id} cannot be added and removed in one bulk operation"
                )));
            }
            transaction.execute(
                "INSERT INTO temp.lectern_bulk_remove_tags(tag_id) VALUES (?1)",
                [id.value()],
            )?;
        }

        let relationships_removed = transaction.execute(
            "DELETE FROM book_tags \
             WHERE book_id IN (SELECT book_id FROM temp.lectern_selected_books) \
               AND tag_id IN (SELECT tag_id FROM temp.lectern_bulk_remove_tags)",
            [],
        )?;
        let relationships_added = transaction.execute(
            "INSERT OR IGNORE INTO book_tags(book_id, tag_id) \
             SELECT selected.book_id, tags.tag_id \
             FROM temp.lectern_selected_books selected \
             CROSS JOIN temp.lectern_bulk_add_tags tags",
            [],
        )?;
        if relationships_added > 0 || relationships_removed > 0 {
            rebuild_selected_tag_projections(&transaction)?;
        }
        let relationships_added = u64::try_from(relationships_added).map_err(|_| {
            StorageError::InvalidCuration("bulk added-row count exceeds u64".into())
        })?;
        let relationships_removed = u64::try_from(relationships_removed).map_err(|_| {
            StorageError::InvalidCuration("bulk removed-row count exceeds u64".into())
        })?;
        transaction.commit()?;
        self.bump_generation();
        Ok(BulkTagResult {
            books_matched,
            relationships_added,
            relationships_removed,
            tags_created,
        })
    }

    /// Removes a compact target set in one durable transaction.
    ///
    /// Cascading foreign keys remove the selected books' asset relationships, cached covers, and
    /// organization relationships. Publication files are never opened, modified, or deleted.
    ///
    /// # Errors
    ///
    /// Returns an error when the target is stale or the deletion cannot commit atomically.
    pub fn remove_books(&mut self, selection: &BookSelection) -> Result<BulkRemovalResult> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (books_matched, _) =
            prepare_selection(&transaction, selection, self.logical_generation.get())?;
        let suspended_triggers = suspend_bulk_removal_triggers(&transaction)?;
        transaction.execute(
            "INSERT INTO books_fts( \
                 books_fts, rowid, title, authors_search, contributors_search, \
                 series, publisher, tags_search \
             ) \
             SELECT \
                 'delete', b.id, b.title, b.authors_search, b.contributors_search, \
                 b.series, b.publisher, b.tags_search \
             FROM books b \
             JOIN temp.lectern_selected_books selected ON selected.book_id = b.id",
            [],
        )?;
        transaction.execute(
            "DELETE FROM book_tags \
             WHERE book_id IN (SELECT book_id FROM temp.lectern_selected_books)",
            [],
        )?;
        transaction.execute(
            "DELETE FROM series_memberships \
             WHERE book_id IN (SELECT book_id FROM temp.lectern_selected_books)",
            [],
        )?;
        transaction.execute(
            "DELETE FROM book_contributors \
             WHERE book_id IN (SELECT book_id FROM temp.lectern_selected_books)",
            [],
        )?;
        transaction.execute(
            "DELETE FROM book_covers \
             WHERE book_id IN (SELECT book_id FROM temp.lectern_selected_books)",
            [],
        )?;
        transaction.execute(
            "DELETE FROM book_assets \
             WHERE book_id IN (SELECT book_id FROM temp.lectern_selected_books)",
            [],
        )?;
        let books_removed = u64::try_from(transaction.execute(
            "DELETE FROM books \
             WHERE id IN (SELECT book_id FROM temp.lectern_selected_books)",
            [],
        )?)
        .map_err(|_| StorageError::InvalidCuration("bulk removed-row count exceeds u64".into()))?;
        if books_removed != books_matched {
            return Err(StorageError::Integrity(format!(
                "bulk removal matched {books_matched} books but removed {books_removed}"
            )));
        }
        restore_bulk_removal_triggers(&transaction, &suspended_triggers)?;
        transaction.commit()?;
        if books_removed > 0 {
            self.bump_generation();
        }
        Ok(BulkRemovalResult { books_removed })
    }

    /// Loads complete editable metadata and every asset for one logical book.
    ///
    /// # Errors
    ///
    /// Returns an error when the book query cannot be executed, an asset cannot be decoded, or a
    /// stored book violates the one-or-more-assets invariant.
    pub fn get_book(&self, id: BookId) -> Result<Option<Book>> {
        let mut statement = self.connection.prepare_cached(
            "SELECT b.id, b.title, b.authors, b.series, b.publisher, b.language, b.description, \
                    a.id, a.format, a.storage_mode, a.health, a.path_encoding, a.path \
             FROM books b LEFT JOIN book_assets a ON a.book_id = b.id \
             WHERE b.id = ?1 ORDER BY a.format, a.id",
        )?;
        let mut rows = statement.query([id.value()])?;
        let mut book = None::<Book>;

        while let Some(row) = rows.next()? {
            if book.is_none() {
                book = Some(Book {
                    id: BookId::new(row.get(0)?),
                    title: row.get(1)?,
                    authors: row.get(2)?,
                    series: row.get(3)?,
                    contributors: Vec::new(),
                    series_membership: None,
                    tags: Vec::new(),
                    identifiers: Vec::new(),
                    publisher: row.get(4)?,
                    language: row.get(5)?,
                    description: row.get(6)?,
                    assets: Vec::new(),
                });
            }

            let asset_id = row.get::<_, Option<i64>>(7)?.ok_or_else(|| {
                StorageError::Integrity(format!("book {id} does not contain an asset"))
            })?;
            let format_value = row.get::<_, String>(8)?;
            let storage_value = row.get::<_, String>(9)?;
            let health_value = row.get::<_, String>(10)?;
            let path_encoding = row.get::<_, String>(11)?;
            let path_bytes = row.get::<_, Vec<u8>>(12)?;
            let asset = BookAsset {
                id: AssetId::new(asset_id),
                format: decode_format(&format_value)?,
                storage: decode_storage(&storage_value)?,
                health: decode_health(&health_value)?,
                path: decode_path(&path_encoding, path_bytes)?,
            };
            if let Some(book) = &mut book {
                book.assets.push(asset);
            }
        }

        if let Some(book) = &mut book {
            let (contributors, series, tags, identifiers) =
                organisation::load_book_curation(&self.connection, book.id)?;
            book.contributors = contributors;
            book.series_membership = series;
            book.tags = tags;
            book.identifiers = identifiers;
        }

        Ok(book)
    }

    /// Persists editable metadata for an existing logical book.
    ///
    /// Asset changes use dedicated asset operations so a metadata save cannot accidentally
    /// replace, detach, or relink files.
    ///
    /// # Errors
    ///
    /// Returns an error when the update fails or the book no longer exists.
    pub fn save_book(&mut self, book: &Book) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let changed = transaction.execute(
            "UPDATE books SET title = ?1, sort_title = ?2, \
             publisher = ?3, language = ?4, description = ?5, modified_at = unixepoch() \
             WHERE id = ?6",
            params![
                book.title.trim(),
                sortable(&book.title),
                optional_text(book.publisher.as_deref()),
                optional_text(book.language.as_deref()),
                optional_text(book.description.as_deref()),
                book.id.value(),
            ],
        )?;
        if changed == 0 {
            return Err(StorageError::BookNotFound(book.id));
        }
        organisation::replace_flattened_organisation(
            &transaction,
            book.id.value(),
            &book.authors,
            book.series.as_deref(),
        )?;
        transaction.commit()?;
        self.bump_generation();
        Ok(())
    }

    /// Atomically persists ordinary metadata and normalized curation relationships.
    ///
    /// Existing and newly named contributors, series, and tags are resolved under the same
    /// immediate transaction. Asset rows and publication files are never read or changed.
    ///
    /// # Errors
    ///
    /// Returns an error when the book or an existing entity is absent, input validation fails,
    /// credit ordering is invalid, or any database operation cannot commit.
    pub fn save_book_edit(&mut self, edit: &BookEdit) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        organisation::save_book_edit(&transaction, edit)?;
        transaction.commit()?;
        self.bump_generation();
        Ok(())
    }

    /// Returns at most fifty contributor identity-prefix matches with selected values first.
    ///
    /// # Errors
    ///
    /// Returns an error when the prefix violates the contributor-name input bound or the indexed
    /// vocabulary query fails.
    pub fn autocomplete_contributors(
        &self,
        prefix: &str,
        selected: &[ContributorId],
        limit: u32,
    ) -> Result<Vec<ContributorUsage>> {
        organisation::autocomplete_contributors(&self.connection, prefix, selected, limit)
    }

    /// Returns at most fifty series identity-prefix matches with selected values first.
    ///
    /// # Errors
    ///
    /// Returns an error when the prefix violates the series-name input bound or the indexed
    /// vocabulary query fails.
    pub fn autocomplete_series(
        &self,
        prefix: &str,
        selected: &[SeriesId],
        limit: u32,
    ) -> Result<Vec<SeriesUsage>> {
        organisation::autocomplete_series(&self.connection, prefix, selected, limit)
    }

    /// Reports whether an exact series number is unused by every other book in the series.
    ///
    /// # Errors
    ///
    /// Returns an error when the indexed lookup cannot be completed.
    pub fn series_index_is_available(
        &self,
        series: SeriesId,
        index: SeriesIndex,
        excluding_book: BookId,
    ) -> Result<bool> {
        organisation::series_index_is_available(&self.connection, series, index, excluding_book)
    }

    /// Returns at most fifty tag identity-prefix matches with selected values first.
    ///
    /// # Errors
    ///
    /// Returns an error when the prefix violates the tag-name input bound or the indexed
    /// vocabulary query fails.
    pub fn autocomplete_tags(
        &self,
        prefix: &str,
        selected: &[TagId],
        limit: u32,
    ) -> Result<Vec<TagUsage>> {
        organisation::autocomplete_tags(&self.connection, prefix, selected, limit)
    }

    /// Returns at most fifty identifier-type identity-prefix matches.
    ///
    /// Stored defaults remain available even before any book uses them.
    ///
    /// # Errors
    ///
    /// Returns an error when the prefix violates the identifier-type input bound or the indexed
    /// vocabulary query fails.
    pub fn autocomplete_identifier_types(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<Vec<IdentifierTypeUsage>> {
        organisation::autocomplete_identifier_types(&self.connection, prefix, limit)
    }

    /// Returns at most one hundred normalized-name contributor rows for vocabulary management.
    ///
    /// # Errors
    ///
    /// Returns an error when the prefix is invalid or the bounded indexed query fails.
    pub fn search_contributors(
        &self,
        prefix: &str,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<ContributorUsage>> {
        organisation::search_contributors(&self.connection, prefix, offset, limit)
    }

    /// Returns at most one hundred normalized-name series rows for vocabulary management.
    ///
    /// # Errors
    ///
    /// Returns an error when the prefix is invalid or the bounded indexed query fails.
    pub fn search_series(&self, prefix: &str, offset: u64, limit: u32) -> Result<Vec<SeriesUsage>> {
        organisation::search_series(&self.connection, prefix, offset, limit)
    }

    /// Returns at most one hundred normalized-name tag rows for vocabulary management.
    ///
    /// # Errors
    ///
    /// Returns an error when the prefix is invalid or the bounded indexed query fails.
    pub fn search_tags(&self, prefix: &str, offset: u64, limit: u32) -> Result<Vec<TagUsage>> {
        organisation::search_tags(&self.connection, prefix, offset, limit)
    }

    /// Counts books and saved projections affected by a contributor mutation.
    ///
    /// # Errors
    ///
    /// Returns an error when the contributor is absent or counts cannot be loaded.
    pub fn contributor_mutation_impact(
        &self,
        id: ContributorId,
    ) -> Result<VocabularyMutationResult> {
        organisation::contributor_impact(&self.connection, id)
    }

    /// Counts books and saved projections affected by a series mutation.
    ///
    /// # Errors
    ///
    /// Returns an error when the series is absent or counts cannot be loaded.
    pub fn series_mutation_impact(&self, id: SeriesId) -> Result<VocabularyMutationResult> {
        organisation::series_impact(&self.connection, id)
    }

    /// Counts books and saved projections affected by a tag mutation.
    ///
    /// # Errors
    ///
    /// Returns an error when the tag is absent or counts cannot be loaded.
    pub fn tag_mutation_impact(&self, id: TagId) -> Result<VocabularyMutationResult> {
        organisation::tag_impact(&self.connection, id)
    }

    /// Renames a contributor and its affected projections in one transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when the entity or name is invalid, the name collides, or the transaction
    /// cannot commit.
    pub fn rename_contributor(
        &mut self,
        id: ContributorId,
        display_name: &str,
        sort_name: &str,
    ) -> Result<VocabularyMutationResult> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = organisation::rename_contributor(&transaction, id, display_name, sort_name)?;
        transaction.commit()?;
        self.bump_generation();
        Ok(result)
    }

    /// Merges a contributor source into an explicit stable target.
    ///
    /// # Errors
    ///
    /// Returns an error when either entity is absent, source equals target, or any relationship,
    /// saved facet, projection, or FTS change cannot commit.
    pub fn merge_contributors(
        &mut self,
        source: ContributorId,
        target: ContributorId,
    ) -> Result<VocabularyMutationResult> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = organisation::merge_contributors(&transaction, source, target)?;
        transaction.commit()?;
        self.bump_generation();
        Ok(result)
    }

    /// Deletes a contributor only when no book or saved search references it.
    ///
    /// # Errors
    ///
    /// Returns an error when the contributor is absent, still used, or cannot be deleted.
    pub fn delete_contributor(&mut self, id: ContributorId) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        organisation::delete_contributor(&transaction, id)?;
        transaction.commit()?;
        self.bump_generation();
        Ok(())
    }

    /// Renames a series and its affected projections in one transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when the entity or name is invalid, the name collides, or the transaction
    /// cannot commit.
    pub fn rename_series(&mut self, id: SeriesId, name: &str) -> Result<VocabularyMutationResult> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = organisation::rename_series(&transaction, id, name)?;
        transaction.commit()?;
        self.bump_generation();
        Ok(result)
    }

    /// Merges a series source into an explicit stable target.
    ///
    /// # Errors
    ///
    /// Returns an error when either entity is absent, source equals target, or any relationship,
    /// saved facet, projection, or FTS change cannot commit.
    pub fn merge_series(
        &mut self,
        source: SeriesId,
        target: SeriesId,
    ) -> Result<VocabularyMutationResult> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = organisation::merge_series(&transaction, source, target)?;
        transaction.commit()?;
        self.bump_generation();
        Ok(result)
    }

    /// Deletes a series only when no book or saved search references it.
    ///
    /// # Errors
    ///
    /// Returns an error when the series is absent, still used, or cannot be deleted.
    pub fn delete_series(&mut self, id: SeriesId) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        organisation::delete_series(&transaction, id)?;
        transaction.commit()?;
        self.bump_generation();
        Ok(())
    }

    /// Renames a tag and its affected search projections in one transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when the entity or name is invalid, the name collides, or the transaction
    /// cannot commit.
    pub fn rename_tag(&mut self, id: TagId, name: &str) -> Result<VocabularyMutationResult> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = organisation::rename_tag(&transaction, id, name)?;
        transaction.commit()?;
        self.bump_generation();
        Ok(result)
    }

    /// Merges a tag source into an explicit stable target and deduplicates every reference.
    ///
    /// # Errors
    ///
    /// Returns an error when either entity is absent, source equals target, or any relationship,
    /// saved facet, projection, or FTS change cannot commit.
    pub fn merge_tags(&mut self, source: TagId, target: TagId) -> Result<VocabularyMutationResult> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = organisation::merge_tags(&transaction, source, target)?;
        transaction.commit()?;
        self.bump_generation();
        Ok(result)
    }

    /// Deletes a tag only when its usage still matches the user-confirmed counts.
    ///
    /// # Errors
    ///
    /// Returns an error when the tag is absent, counts changed, or relationships, saved facets,
    /// projections, and the entity cannot be deleted atomically.
    pub fn delete_tag(
        &mut self,
        id: TagId,
        confirmed: VocabularyMutationResult,
    ) -> Result<VocabularyMutationResult> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = organisation::delete_tag(&transaction, id, confirmed)?;
        transaction.commit()?;
        self.bump_generation();
        Ok(result)
    }

    /// Lists durable saved projections alphabetically by normalized name.
    ///
    /// # Errors
    ///
    /// Returns an error when stored canonical values or facet references cannot be loaded.
    pub fn list_saved_searches(&self) -> Result<Vec<SavedSearch>> {
        organisation::list_saved_searches(&self.connection)
    }

    /// Returns at most one hundred durable saved projections in normalized-name order.
    ///
    /// # Errors
    ///
    /// Returns an error when the prefix is invalid, stored canonical state is corrupt, or the
    /// bounded indexed query cannot be executed.
    pub fn search_saved_searches(
        &self,
        prefix: &str,
        offset: u64,
        limit: u32,
    ) -> Result<Vec<SavedSearch>> {
        organisation::search_saved_searches(&self.connection, prefix, offset, limit)
    }

    /// Creates one named canonical query/filter/sort projection.
    ///
    /// # Errors
    ///
    /// Returns an error when the name or query is invalid, collides, references an absent entity,
    /// or cannot be committed.
    pub fn create_saved_search(
        &mut self,
        name: &str,
        query: &LibraryQuery,
    ) -> Result<SavedSearchId> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = organisation::create_saved_search(&transaction, name, query)?;
        transaction.commit()?;
        self.bump_generation();
        Ok(id)
    }

    /// Explicitly replaces one saved projection while retaining its name and stable ID.
    ///
    /// # Errors
    ///
    /// Returns an error when the saved search is absent, the query is invalid, a facet entity is
    /// absent, or the transaction cannot commit.
    pub fn update_saved_search(&mut self, id: SavedSearchId, query: &LibraryQuery) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        organisation::update_saved_search(&transaction, id, query)?;
        transaction.commit()?;
        self.bump_generation();
        Ok(())
    }

    /// Renames one saved projection without changing its query or stable ID.
    ///
    /// # Errors
    ///
    /// Returns an error when the saved search is absent, the name is invalid or collides, or the
    /// transaction cannot commit.
    pub fn rename_saved_search(&mut self, id: SavedSearchId, name: &str) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        organisation::rename_saved_search(&transaction, id, name)?;
        transaction.commit()?;
        self.bump_generation();
        Ok(())
    }

    /// Deletes one saved projection without changing books or vocabulary.
    ///
    /// # Errors
    ///
    /// Returns an error when the delete cannot be committed.
    pub fn delete_saved_search(&self, id: SavedSearchId) -> Result<bool> {
        let changed = self
            .connection
            .prepare_cached("DELETE FROM saved_searches WHERE id = ?1")?
            .execute([id.value()])?;
        if changed == 1 {
            self.bump_generation();
        }
        Ok(changed == 1)
    }

    /// Attaches one externally referenced publication to an existing logical book.
    ///
    /// Callers must validate the publication before invoking this method. The immediate
    /// transaction preserves the book's metadata, cover, and existing assets while enforcing one
    /// asset per format and global reference-path ownership. The source file is never copied,
    /// moved, or modified.
    ///
    /// # Errors
    ///
    /// Returns an error when the book is absent, already has the requested format, the source is
    /// unavailable, another reference asset owns the path, or the insertion cannot be committed.
    pub fn attach_reference_asset(
        &mut self,
        book: BookId,
        format: BookFormat,
        path: impl AsRef<Path>,
    ) -> Result<AssetId> {
        let path = path.as_ref();
        validate_asset_path(AssetStorage::Reference, path)?;
        let health = inspect_reference_asset(path);
        if health != AssetHealth::Available {
            return Err(StorageError::AttachmentUnavailable(health));
        }
        let (path_encoding, encoded_path) = encode_path(path);

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let book_exists = transaction
            .query_row("SELECT 1 FROM books WHERE id = ?1", [book.value()], |_| {
                Ok(())
            })
            .optional()?
            .is_some();
        if !book_exists {
            return Err(StorageError::BookNotFound(book));
        }

        let existing_format = transaction
            .query_row(
                "SELECT id FROM book_assets WHERE book_id = ?1 AND format = ?2",
                params![book.value(), format.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if existing_format.is_some() {
            return Err(StorageError::BookAlreadyHasFormat { book, format });
        }

        let path_owner = transaction
            .query_row(
                "SELECT id FROM book_assets \
                 WHERE storage_mode = 'reference' AND path_encoding = ?1 AND path = ?2",
                params![path_encoding, &encoded_path],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if let Some(owner) = path_owner {
            return Err(StorageError::ReferencePathInUse(AssetId::new(owner)));
        }

        transaction.execute(
            "INSERT INTO book_assets (book_id, format, storage_mode, health, path_encoding, path) \
             VALUES (?1, ?2, 'reference', 'available', ?3, ?4)",
            params![book.value(), format.as_str(), path_encoding, encoded_path],
        )?;
        let asset = AssetId::new(transaction.last_insert_rowid());
        transaction.commit()?;
        self.bump_generation();
        Ok(asset)
    }

    /// Detaches exactly one file asset from its logical book.
    ///
    /// The immediate transaction resolves the owning book and enforces the one-or-more-assets
    /// invariant before deleting the relationship. Publication bytes, book metadata, and the
    /// cached cover are never modified or deleted.
    ///
    /// # Errors
    ///
    /// Returns an error when the asset is absent, it is the book's last asset, or the transaction
    /// cannot be committed.
    pub fn detach_asset(&mut self, id: AssetId) -> Result<BookId> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let book = transaction
            .query_row(
                "SELECT book_id FROM book_assets WHERE id = ?1",
                [id.value()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(BookId::new)
            .ok_or(StorageError::AssetNotFound(id))?;
        let asset_count: i64 = transaction.query_row(
            "SELECT count(*) FROM book_assets WHERE book_id = ?1",
            [book.value()],
            |row| row.get(0),
        )?;
        if asset_count <= 1 {
            return Err(StorageError::LastAssetDetach { asset: id, book });
        }

        let changed = transaction.execute("DELETE FROM book_assets WHERE id = ?1", [id.value()])?;
        if changed != 1 {
            return Err(StorageError::AssetNotFound(id));
        }
        transaction.commit()?;
        self.bump_generation();
        Ok(book)
    }

    /// Removes one logical book and its stored library data.
    ///
    /// The database cascades this deletion to the book's asset records and cached cover. Referenced
    /// or managed publication files are never deleted by this operation.
    ///
    /// # Errors
    ///
    /// Returns an error when the deletion cannot be executed.
    pub fn remove_book(&self, id: BookId) -> Result<bool> {
        let changed = self
            .connection
            .prepare_cached("DELETE FROM books WHERE id = ?1")?
            .execute([id.value()])?;
        if changed == 1 {
            self.bump_generation();
        }
        Ok(changed == 1)
    }

    /// Checks every externally referenced asset and records only changed health states.
    ///
    /// The scan uses filesystem metadata plus a file-open check. It intentionally does not parse
    /// publication contents, compute hashes, or rewrite rows whose health state is unchanged.
    /// Managed assets are omitted because their library-root resolution is owned by future managed
    /// storage support.
    ///
    /// # Errors
    ///
    /// Returns an error when asset paths cannot be decoded or changed health states cannot be
    /// persisted.
    pub fn rescan_reference_assets(&mut self) -> Result<AssetHealthReport> {
        let mut statement = self.connection.prepare_cached(
            "SELECT id, health, path_encoding, path FROM book_assets \
             WHERE storage_mode = 'reference' ORDER BY id",
        )?;
        let mut rows = statement.query([])?;
        let mut assets = Vec::new();
        while let Some(row) = rows.next()? {
            let health = decode_health(&row.get::<_, String>(1)?)?;
            let path = decode_path(&row.get::<_, String>(2)?, row.get(3)?)?;
            assets.push(ReferenceAsset {
                id: AssetId::new(row.get(0)?),
                health,
                path,
            });
        }
        drop(rows);
        drop(statement);

        let mut report = AssetHealthReport::default();
        let mut changed = Vec::new();
        for asset in assets {
            let health = inspect_reference_asset(&asset.path);
            report.checked += 1;
            match health {
                AssetHealth::Available => report.available += 1,
                AssetHealth::Missing => report.missing += 1,
                AssetHealth::Unreadable => report.unreadable += 1,
                AssetHealth::Unknown => unreachable!("a scan must produce a concrete health"),
            }
            if health != asset.health {
                changed.push((asset.id, health));
            }
        }
        report.changed = changed.len();
        if changed.is_empty() {
            return Ok(report);
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut update = transaction.prepare(
            "UPDATE book_assets SET health = ?1, modified_at = unixepoch() WHERE id = ?2",
        )?;
        for (id, health) in changed {
            update.execute(params![health.as_str(), id.value()])?;
        }
        drop(update);
        transaction.commit()?;
        self.bump_generation();
        Ok(report)
    }

    /// Recovers an unavailable reference asset at a validated replacement path.
    ///
    /// Callers must validate the replacement publication before invoking this method. The supplied
    /// `replacement_format` is checked against the stored asset format inside the same immediate
    /// transaction that protects path uniqueness. On success, the asset retains its ID and its
    /// owning book, metadata, and cover are untouched.
    ///
    /// # Errors
    ///
    /// Returns an error when the asset is absent, managed, or still available; the validated format
    /// differs; the replacement is unavailable; or another asset owns the replacement path.
    pub fn relink_reference_asset(
        &mut self,
        id: AssetId,
        replacement_path: impl AsRef<Path>,
        replacement_format: BookFormat,
    ) -> Result<()> {
        let result = self.update_reference_asset_path(
            id,
            replacement_path.as_ref(),
            replacement_format,
            ReferencePathUpdate::Relink,
        );
        if result.is_ok() {
            self.bump_generation();
        }
        result
    }

    /// Deliberately replaces the path of a validated, externally referenced asset.
    ///
    /// Unlike relink recovery, this operation is available regardless of current file health. It
    /// retains the stable asset ID, logical book, metadata, and cover, and never modifies or deletes
    /// the former or replacement file.
    ///
    /// # Errors
    ///
    /// Returns an error when the asset is absent or managed, the validated format differs, the
    /// replacement is unavailable, another reference asset owns the path, or the transaction fails.
    pub fn replace_reference_asset(
        &mut self,
        id: AssetId,
        replacement_path: impl AsRef<Path>,
        replacement_format: BookFormat,
    ) -> Result<()> {
        let result = self.update_reference_asset_path(
            id,
            replacement_path.as_ref(),
            replacement_format,
            ReferencePathUpdate::Replace,
        );
        if result.is_ok() {
            self.bump_generation();
        }
        result
    }

    /// Creates a transactionally consistent database snapshot at a new path.
    ///
    /// The online `SQLite` backup API includes committed pages that remain in an active WAL without
    /// copying WAL or shared-memory sidecars. The snapshot is built and checked in a reserved
    /// sibling file, synced, and then published without replacing an existing destination.
    ///
    /// # Errors
    ///
    /// Returns an error when the destination already exists, its parent cannot be created, `SQLite`
    /// cannot finish or validate the snapshot, or the completed file cannot be published.
    pub fn backup(&self, destination: impl AsRef<Path>) -> Result<BackupReport> {
        let destination = destination.as_ref();
        let file_name = destination
            .file_name()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| StorageError::InvalidBackupDestination(destination.to_path_buf()))?;
        if destination.exists() {
            return Err(StorageError::BackupDestinationExists(
                destination.to_path_buf(),
            ));
        }

        let parent = destination
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        let temporary = TemporaryBackup::reserve(parent, file_name)?;

        let mut snapshot = Connection::open(temporary.path())?;
        // The destination is an unpublished, newly created file, so rollback journaling cannot
        // protect any prior user data. Avoid the journal and SQLite's intermediate syncs, then
        // explicitly sync the completed, validated snapshot once before publishing it. This keeps
        // the durability boundary while avoiding redundant writes for large libraries.
        snapshot.pragma_update(None, "journal_mode", "OFF")?;
        snapshot.pragma_update(None, "synchronous", "OFF")?;
        {
            let backup = rusqlite::backup::Backup::new(&self.connection, &mut snapshot)?;
            // Release SQLite's source lock between bounded 8 MiB chunks. Smaller chunks add one
            // millisecond of unconditional scheduler sleep each, which dominates large snapshots;
            // this remains responsive to other connections without adding hundreds of pauses.
            backup.run_to_completion(BACKUP_PAGES_PER_STEP, Duration::from_millis(1), None)?;
        }

        let integrity: String = snapshot.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if !integrity.eq_ignore_ascii_case("ok") {
            return Err(StorageError::Integrity(format!(
                "backup quick check failed: {integrity}"
            )));
        }
        let books =
            validated_count(
                snapshot.query_row("SELECT count(*) FROM books", [], |row| row.get(0))?,
            )?;
        drop(snapshot);
        OpenOptions::new()
            .read(true)
            .open(temporary.path())?
            .sync_all()?;

        match std::fs::hard_link(temporary.path(), destination) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(StorageError::BackupDestinationExists(
                    destination.to_path_buf(),
                ));
            }
            Err(error) => return Err(error.into()),
        }
        let bytes = std::fs::metadata(destination)?.len();

        Ok(BackupReport {
            destination: destination.to_path_buf(),
            bytes,
            books,
        })
    }

    /// Runs read-only database, index, relationship, and referenced-file checks.
    ///
    /// # Errors
    ///
    /// Returns an error only when the diagnostic queries themselves cannot run. Findings such as
    /// corrupt pages, an inconsistent FTS index, or unavailable files are returned in the report.
    pub fn diagnostics(&self) -> Result<LibraryDiagnostics> {
        let schema_version = self
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))?;

        let sqlite_integrity_errors = collect_integrity_errors(&self.connection)?;
        let foreign_key_violations = collect_foreign_key_violations(&self.connection)?;

        let fts_error = self
            .connection
            .execute(
                "INSERT INTO books_fts(books_fts, rank) VALUES ('integrity-check', 1)",
                [],
            )
            .err()
            .map(|error| error.to_string());

        let books_without_assets = diagnostic_count(
            &self.connection,
            "SELECT count(*) FROM books b \
             WHERE NOT EXISTS (SELECT 1 FROM book_assets a WHERE a.book_id = b.id)",
        )?;
        let duplicate_book_formats = diagnostic_count(
            &self.connection,
            "SELECT count(*) FROM ( \
                 SELECT 1 FROM book_assets GROUP BY book_id, format HAVING count(*) > 1 \
             )",
        )?;
        let duplicate_reference_paths = diagnostic_count(
            &self.connection,
            "SELECT count(*) FROM ( \
                 SELECT 1 FROM book_assets WHERE storage_mode = 'reference' \
                 GROUP BY path_encoding, path HAVING count(*) > 1 \
             )",
        )?;
        let invalid_asset_relationships = diagnostic_count(
            &self.connection,
            "SELECT count(*) FROM book_assets a \
             LEFT JOIN books b ON b.id = a.book_id \
             WHERE b.id IS NULL \
                OR a.format NOT IN ('epub', 'pdf') \
                OR a.storage_mode NOT IN ('reference', 'managed') \
                OR a.health NOT IN ('unknown', 'available', 'missing', 'unreadable') \
                OR a.path_encoding NOT IN ('utf8', 'unix', 'windows') \
                OR length(a.path) = 0 \
                OR (a.storage_mode = 'managed' AND a.path_encoding <> 'utf8') \
                OR (a.path_encoding = 'windows' AND length(a.path) % 2 <> 0)",
        )?;
        let unchecked_managed_assets = diagnostic_count(
            &self.connection,
            "SELECT count(*) FROM book_assets WHERE storage_mode = 'managed'",
        )?;

        let referenced_files = collect_referenced_file_diagnostics(&self.connection)?;

        Ok(LibraryDiagnostics {
            schema_version,
            supported_schema_version: SCHEMA_VERSION,
            sqlite_integrity_errors,
            foreign_key_violations,
            fts_error,
            books_without_assets,
            duplicate_book_formats,
            duplicate_reference_paths,
            invalid_asset_relationships,
            referenced_files,
            unchecked_managed_assets,
        })
    }

    /// Returns compact library and asset counts using one read transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when the aggregate query fails or `SQLite` returns a negative count.
    pub fn stats(&self) -> Result<LibraryStats> {
        let transaction = self.connection.unchecked_transaction()?;
        let counts = transaction.query_row(
            "SELECT \
                 (SELECT count(*) FROM books), \
                 count(*), \
                 (SELECT count(*) FROM book_covers), \
                 count(*) FILTER (WHERE format = 'epub'), \
                 count(*) FILTER (WHERE format = 'pdf'), \
                 count(*) FILTER (WHERE storage_mode = 'reference'), \
                 count(*) FILTER (WHERE storage_mode = 'managed'), \
                 count(*) FILTER (WHERE health = 'unknown'), \
                 count(*) FILTER (WHERE health = 'available'), \
                 count(*) FILTER (WHERE health = 'missing'), \
                 count(*) FILTER (WHERE health = 'unreadable') \
             FROM book_assets",
            [],
            |row| {
                Ok([
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                ])
            },
        )?;
        transaction.commit()?;
        let [
            books,
            assets,
            covers,
            epub_assets,
            pdf_assets,
            referenced_assets,
            managed_assets,
            unknown_assets,
            available_assets,
            missing_assets,
            unreadable_assets,
        ] = counts;
        Ok(LibraryStats {
            books: validated_count(books)?,
            assets: validated_count(assets)?,
            covers: validated_count(covers)?,
            epub_assets: validated_count(epub_assets)?,
            pdf_assets: validated_count(pdf_assets)?,
            referenced_assets: validated_count(referenced_assets)?,
            managed_assets: validated_count(managed_assets)?,
            unknown_assets: validated_count(unknown_assets)?,
            available_assets: validated_count(available_assets)?,
            missing_assets: validated_count(missing_assets)?,
            unreadable_assets: validated_count(unreadable_assets)?,
        })
    }

    fn update_reference_asset_path(
        &mut self,
        id: AssetId,
        replacement_path: &Path,
        replacement_format: BookFormat,
        operation: ReferencePathUpdate,
    ) -> Result<()> {
        validate_asset_path(AssetStorage::Reference, replacement_path)?;
        let replacement_health = inspect_reference_asset(replacement_path);
        if replacement_health != AssetHealth::Available {
            return Err(StorageError::ReplacementUnavailable(replacement_health));
        }
        let (path_encoding, path) = encode_path(replacement_path);

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (format, storage, current_path_encoding, current_path) = transaction
            .query_row(
                "SELECT format, storage_mode, path_encoding, path \
                 FROM book_assets WHERE id = ?1",
                [id.value()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                    ))
                },
            )
            .optional()?
            .ok_or(StorageError::AssetNotFound(id))?;
        let format = decode_format(&format)?;
        if format != replacement_format {
            return Err(match operation {
                ReferencePathUpdate::Relink => StorageError::RelinkFormatMismatch {
                    expected: format,
                    found: replacement_format,
                },
                ReferencePathUpdate::Replace => StorageError::ReplacementFormatMismatch {
                    expected: format,
                    found: replacement_format,
                },
            });
        }
        if decode_storage(&storage)? != AssetStorage::Reference {
            return Err(StorageError::AssetNotReference(id));
        }
        let current_path = decode_path(&current_path_encoding, current_path)?;
        if matches!(operation, ReferencePathUpdate::Relink)
            && inspect_reference_asset(&current_path) == AssetHealth::Available
        {
            return Err(StorageError::RelinkAssetAvailable(id));
        }
        let replacement_health = inspect_reference_asset(replacement_path);
        if replacement_health != AssetHealth::Available {
            return Err(StorageError::ReplacementUnavailable(replacement_health));
        }

        let owner = transaction
            .query_row(
                "SELECT id FROM book_assets \
                 WHERE storage_mode = 'reference' AND path_encoding = ?1 AND path = ?2 \
                 AND id <> ?3",
                params![path_encoding, path, id.value()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if let Some(owner) = owner {
            return Err(StorageError::ReferencePathInUse(AssetId::new(owner)));
        }

        transaction.execute(
            "UPDATE book_assets SET path_encoding = ?1, path = ?2, health = 'available', \
             modified_at = unixepoch() WHERE id = ?3",
            params![path_encoding, path, id.value()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Loads a cached JPEG cover thumbnail.
    ///
    /// # Errors
    ///
    /// Returns an error when the cover query cannot be executed or decoded.
    pub fn load_cover(&self, id: BookId) -> Result<Option<Vec<u8>>> {
        let cover = self
            .connection
            .prepare_cached("SELECT jpeg FROM book_covers WHERE book_id = ?1")?
            .query_row([id.value()], |row| row.get(0))
            .optional()?;
        Ok(cover)
    }
}

struct TemporaryBackup {
    path: PathBuf,
}

impl TemporaryBackup {
    fn reserve(parent: &Path, destination_name: &std::ffi::OsStr) -> Result<Self> {
        for _ in 0..1_024 {
            let id = NEXT_BACKUP_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let mut name = OsString::from(".");
            name.push(destination_name);
            name.push(format!(".lectern-backup-{}-{id}.tmp", std::process::id()));
            let path = parent.join(name);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    drop(file);
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(StorageError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not reserve a temporary backup path",
        )))
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryBackup {
    fn drop(&mut self) {
        let _removed = std::fs::remove_file(&self.path);
    }
}

fn diagnostic_count(connection: &Connection, sql: &str) -> Result<u64> {
    validated_count(connection.query_row(sql, [], |row| row.get(0))?)
}

fn collect_integrity_errors(connection: &Connection) -> Result<Vec<String>> {
    let mut statement = connection.prepare("PRAGMA integrity_check")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut errors = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    if errors.len() == 1 && errors[0].eq_ignore_ascii_case("ok") {
        errors.clear();
    }
    Ok(errors)
}

fn collect_foreign_key_violations(connection: &Connection) -> Result<u64> {
    let mut statement = connection.prepare("PRAGMA foreign_key_check")?;
    let mut rows = statement.query([])?;
    let mut count = 0_u64;
    while rows.next()?.is_some() {
        count += 1;
    }
    Ok(count)
}

fn collect_referenced_file_diagnostics(
    connection: &Connection,
) -> Result<ReferencedFileDiagnostics> {
    let mut invalid_paths = 0_u64;
    let mut assets = Vec::new();
    let mut statement = connection.prepare(
        "SELECT health, path_encoding, path FROM book_assets \
         WHERE storage_mode = 'reference' ORDER BY id",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let stored_health = decode_health(&row.get::<_, String>(0)?)?;
        let path_encoding = row.get::<_, String>(1)?;
        let path = match decode_path(&path_encoding, row.get(2)?) {
            Ok(path) => path,
            Err(StorageError::InvalidPathEncoding(_) | StorageError::InvalidPathData(_)) => {
                invalid_paths += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        assets.push((stored_health, path));
    }
    let mut diagnostics = assets
        .par_iter()
        .fold(ReferencedFileDiagnostics::default, |mut report, asset| {
            let (stored_health, path) = asset;
            let observed_health = inspect_reference_asset(path);
            report.checked += 1;
            match observed_health {
                AssetHealth::Available => report.available += 1,
                AssetHealth::Missing => report.missing += 1,
                AssetHealth::Unreadable => report.unreadable += 1,
                AssetHealth::Unknown => {
                    unreachable!("a diagnostic must produce a concrete health")
                }
            }
            if *stored_health != observed_health {
                report.stale_health += 1;
            }
            report
        })
        .reduce(ReferencedFileDiagnostics::default, merge_file_diagnostics);
    diagnostics.checked += invalid_paths;
    diagnostics.invalid_paths = invalid_paths;
    Ok(diagnostics)
}

fn merge_file_diagnostics(
    left: ReferencedFileDiagnostics,
    right: ReferencedFileDiagnostics,
) -> ReferencedFileDiagnostics {
    ReferencedFileDiagnostics {
        checked: left.checked + right.checked,
        available: left.available + right.available,
        missing: left.missing + right.missing,
        unreadable: left.unreadable + right.unreadable,
        invalid_paths: left.invalid_paths + right.invalid_paths,
        stale_health: left.stale_health + right.stale_health,
    }
}

fn validated_count(count: i64) -> Result<u64> {
    u64::try_from(count).map_err(|_| StorageError::InvalidCount(count))
}

fn configure_persistent_database(connection: &Connection) -> Result<()> {
    let journal_mode = connection.query_row("PRAGMA journal_mode = WAL", [], |row| {
        row.get::<_, String>(0)
    })?;

    // SQLite may decline WAL on an unsupported filesystem. In that case, explicitly activate a
    // durable rollback journal instead of accepting modes such as MEMORY or OFF.
    if !journal_mode.eq_ignore_ascii_case("wal") {
        let fallback = connection.query_row("PRAGMA journal_mode = DELETE", [], |row| {
            row.get::<_, String>(0)
        })?;
        if !fallback.eq_ignore_ascii_case("delete") {
            return Err(StorageError::Integrity(format!(
                "SQLite could not activate WAL or rollback journaling (reported '{fallback}')"
            )));
        }
    }

    // FULL is safe in both WAL and rollback-journal modes.
    connection.pragma_update(None, "synchronous", "FULL")?;
    let synchronous =
        connection.pragma_query_value(None, "synchronous", |row| row.get::<_, i64>(0))?;
    if synchronous != 2 {
        return Err(StorageError::Integrity(
            "SQLite could not activate full synchronization".into(),
        ));
    }
    Ok(())
}

fn initialize_schema(connection: &mut Connection) -> Result<()> {
    let observed = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    match observed {
        SCHEMA_VERSION => return Ok(()),
        0..=8 => {}
        unsupported => return Err(StorageError::UnsupportedSchema(unsupported)),
    }

    connection.pragma_update(None, "foreign_keys", "OFF")?;
    let initialization = initialize_schema_transaction(connection);
    let restore = connection.pragma_update(None, "foreign_keys", "ON");

    initialization?;
    restore?;
    let enabled: i64 = connection.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
    if enabled != 1 {
        return Err(StorageError::Integrity(
            "foreign-key enforcement could not be restored".into(),
        ));
    }
    Ok(())
}

fn initialize_schema_transaction(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version = transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let changed = match version {
        0 => {
            transaction.execute_batch(SCHEMA)?;
            organisation::migrate_v5_to_v6(&transaction)?;
            organisation::migrate_v6_to_v7(&transaction)?;
            organisation::migrate_v7_to_v8(&transaction)?;
            organisation::migrate_v8_to_v9(&transaction)?;
            true
        }
        1 | 2 => {
            transaction.execute_batch(MIGRATE_1_OR_2_TO_3)?;
            transaction.execute_batch(MIGRATE_3_TO_4)?;
            transaction.execute_batch(MIGRATE_4_TO_5)?;
            organisation::migrate_v5_to_v6(&transaction)?;
            organisation::migrate_v6_to_v7(&transaction)?;
            organisation::migrate_v7_to_v8(&transaction)?;
            organisation::migrate_v8_to_v9(&transaction)?;
            true
        }
        3 => {
            transaction.execute_batch(MIGRATE_3_TO_4)?;
            transaction.execute_batch(MIGRATE_4_TO_5)?;
            organisation::migrate_v5_to_v6(&transaction)?;
            organisation::migrate_v6_to_v7(&transaction)?;
            organisation::migrate_v7_to_v8(&transaction)?;
            organisation::migrate_v8_to_v9(&transaction)?;
            true
        }
        4 => {
            transaction.execute_batch(MIGRATE_4_TO_5)?;
            organisation::migrate_v5_to_v6(&transaction)?;
            organisation::migrate_v6_to_v7(&transaction)?;
            organisation::migrate_v7_to_v8(&transaction)?;
            organisation::migrate_v8_to_v9(&transaction)?;
            true
        }
        5 => {
            organisation::migrate_v5_to_v6(&transaction)?;
            organisation::migrate_v6_to_v7(&transaction)?;
            organisation::migrate_v7_to_v8(&transaction)?;
            organisation::migrate_v8_to_v9(&transaction)?;
            true
        }
        6 => {
            organisation::migrate_v6_to_v7(&transaction)?;
            organisation::migrate_v7_to_v8(&transaction)?;
            organisation::migrate_v8_to_v9(&transaction)?;
            true
        }
        7 => {
            organisation::migrate_v7_to_v8(&transaction)?;
            organisation::migrate_v8_to_v9(&transaction)?;
            true
        }
        8 => {
            organisation::migrate_v8_to_v9(&transaction)?;
            true
        }
        SCHEMA_VERSION => false,
        unsupported => return Err(StorageError::UnsupportedSchema(unsupported)),
    };

    if changed {
        validate_schema(&transaction)?;
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }
    transaction.commit()?;
    Ok(())
}

fn validate_schema(transaction: &Transaction<'_>) -> Result<()> {
    let mut foreign_keys = transaction.prepare("PRAGMA foreign_key_check")?;
    if foreign_keys.exists([])? {
        return Err(StorageError::Integrity(
            "schema migration left a foreign-key violation".into(),
        ));
    }
    drop(foreign_keys);

    let missing_asset = transaction.query_row(
        "SELECT EXISTS( \
             SELECT 1 FROM books b \
             WHERE NOT EXISTS (SELECT 1 FROM book_assets a WHERE a.book_id = b.id) \
         )",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if missing_asset {
        return Err(StorageError::Integrity(
            "schema migration left a book without an asset".into(),
        ));
    }

    let stale_summary = transaction.query_row(
        "SELECT EXISTS( \
             SELECT 1 FROM books b \
             WHERE b.has_cover IS NOT EXISTS( \
                       SELECT 1 FROM book_covers c WHERE c.book_id = b.id \
                   ) \
                OR b.has_file_issue IS NOT EXISTS( \
                       SELECT 1 FROM book_assets a \
                       WHERE a.book_id = b.id \
                         AND a.health IN ('missing', 'unreadable') \
                   ) \
         )",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if stale_summary {
        return Err(StorageError::Integrity(
            "schema migration left stale book summary state".into(),
        ));
    }

    organisation::validate_organisation_schema(transaction)?;

    transaction.execute(
        "INSERT INTO books_fts(books_fts, rank) VALUES ('integrity-check', 1)",
        [],
    )?;
    Ok(())
}

#[derive(Clone, Copy)]
struct MetadataInput<'a> {
    title: &'a str,
    authors: &'a str,
    series: Option<&'a str>,
    publisher: Option<&'a str>,
    language: Option<&'a str>,
    description: Option<&'a str>,
    imported_organisation: Option<&'a lectern_core::organisation::ImportedOrganisation>,
}

impl<'a> From<&'a BookMetadataDraft> for MetadataInput<'a> {
    fn from(book: &'a BookMetadataDraft) -> Self {
        Self {
            title: &book.title,
            authors: &book.authors,
            series: book.series.as_deref(),
            publisher: book.publisher.as_deref(),
            language: book.language.as_deref(),
            description: book.description.as_deref(),
            imported_organisation: book.imported_organisation.as_ref(),
        }
    }
}

impl<'a> From<&'a BookDraft> for MetadataInput<'a> {
    fn from(book: &'a BookDraft) -> Self {
        Self {
            title: &book.title,
            authors: &book.authors,
            series: book.series.as_deref(),
            publisher: book.publisher.as_deref(),
            language: book.language.as_deref(),
            description: book.description.as_deref(),
            imported_organisation: None,
        }
    }
}

#[derive(Clone, Copy)]
struct AssetInput<'a> {
    format: BookFormat,
    storage: AssetStorage,
    path: &'a Path,
}

impl<'a> From<&'a BookAssetDraft> for AssetInput<'a> {
    fn from(asset: &'a BookAssetDraft) -> Self {
        Self {
            format: asset.format,
            storage: asset.storage,
            path: &asset.path,
        }
    }
}

struct EncodedAsset {
    format: BookFormat,
    storage: AssetStorage,
    path_encoding: &'static str,
    path: Vec<u8>,
}

struct ReferenceAsset {
    id: AssetId,
    health: AssetHealth,
    path: PathBuf,
}

struct ImportStatements<'connection> {
    find_reference_owner: Statement<'connection>,
    insert_book: Statement<'connection>,
    replace_metadata: Statement<'connection>,
    upsert_asset: Statement<'connection>,
    upsert_cover: Statement<'connection>,
}

impl<'connection> ImportStatements<'connection> {
    fn prepare(transaction: &'connection Transaction<'_>) -> Result<Self> {
        Ok(Self {
            find_reference_owner: transaction.prepare(
                "SELECT book_id FROM book_assets \
                 WHERE storage_mode = 'reference' AND path_encoding = ?1 AND path = ?2",
            )?,
            insert_book: transaction.prepare(
                "INSERT INTO books ( \
                     title, sort_title, authors, sort_authors, series, publisher, language, \
                     description \
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?,
            replace_metadata: transaction.prepare(
                "UPDATE books SET \
                     title = ?1, sort_title = ?2, authors = ?3, sort_authors = ?4, \
                     series = ?5, publisher = ?6, language = ?7, description = ?8, \
                     modified_at = unixepoch() WHERE id = ?9",
            )?,
            upsert_asset: transaction.prepare(
                "INSERT INTO book_assets ( \
                     book_id, format, storage_mode, path_encoding, path \
                 ) VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(book_id, format) DO UPDATE SET \
                     storage_mode = excluded.storage_mode, \
                     path_encoding = excluded.path_encoding, \
                     path = excluded.path, \
                     modified_at = unixepoch() \
                 WHERE book_assets.storage_mode IS NOT excluded.storage_mode \
                    OR book_assets.path_encoding IS NOT excluded.path_encoding \
                    OR book_assets.path IS NOT excluded.path",
            )?,
            upsert_cover: transaction.prepare(
                "INSERT INTO book_covers(book_id, jpeg) VALUES (?1, ?2) \
                 ON CONFLICT(book_id) DO UPDATE SET jpeg = excluded.jpeg",
            )?,
        })
    }
}

fn upsert_book<'a>(
    transaction: &Transaction<'_>,
    statements: &mut ImportStatements<'_>,
    metadata: MetadataInput<'_>,
    assets: impl IntoIterator<Item = AssetInput<'a>>,
    cover: Option<&[u8]>,
    metadata_policy: ReimportMetadataPolicy,
) -> Result<BookId> {
    let assets = prepare_assets(assets)?;
    let mut owner = None::<i64>;
    for asset in assets
        .iter()
        .filter(|asset| asset.storage == AssetStorage::Reference)
    {
        let existing = statements
            .find_reference_owner
            .query_row(params![asset.path_encoding, asset.path], |row| row.get(0))
            .optional()?;
        if let Some(existing) = existing {
            if owner.is_some_and(|owner| owner != existing) {
                return Err(StorageError::ConflictingAssetOwners);
            }
            owner = Some(existing);
        }
    }

    let id = if let Some(id) = owner {
        if metadata_policy == ReimportMetadataPolicy::ReplaceExisting {
            statements.replace_metadata.execute(params![
                metadata.title.trim(),
                sortable(metadata.title),
                metadata.authors.trim(),
                sortable(metadata.authors),
                optional_text(metadata.series),
                optional_text(metadata.publisher),
                optional_text(metadata.language),
                optional_text(metadata.description),
                id,
            ])?;
            organisation::replace_flattened_organisation(
                transaction,
                id,
                metadata.authors,
                metadata.series,
            )?;
        }
        id
    } else {
        statements.insert_book.execute(params![
            metadata.title.trim(),
            sortable(metadata.title),
            metadata.authors.trim(),
            sortable(metadata.authors),
            optional_text(metadata.series),
            optional_text(metadata.publisher),
            optional_text(metadata.language),
            optional_text(metadata.description),
        ])?;
        transaction.last_insert_rowid()
    };

    if owner.is_none() {
        organisation::replace_imported_organisation(
            transaction,
            id,
            metadata.authors,
            metadata.series,
            metadata.imported_organisation,
        )?;
    }

    for asset in assets {
        statements.upsert_asset.execute(params![
            id,
            asset.format.as_str(),
            asset.storage.as_str(),
            asset.path_encoding,
            asset.path,
        ])?;
    }
    if let Some(cover) = cover {
        statements.upsert_cover.execute(params![id, cover])?;
    }
    Ok(BookId::new(id))
}

fn prepare_assets<'a>(
    assets: impl IntoIterator<Item = AssetInput<'a>>,
) -> Result<Vec<EncodedAsset>> {
    let mut prepared = Vec::<EncodedAsset>::new();
    for asset in assets {
        if prepared
            .iter()
            .any(|existing| existing.format == asset.format)
        {
            return Err(StorageError::DuplicateAssetFormat(asset.format));
        }
        validate_asset_path(asset.storage, asset.path)?;
        let (path_encoding, path) = encode_path(asset.path);
        if asset.storage == AssetStorage::Reference
            && prepared.iter().any(|existing| {
                existing.storage == AssetStorage::Reference
                    && existing.path_encoding == path_encoding
                    && existing.path == path
            })
        {
            return Err(StorageError::DuplicateAssetPath);
        }
        prepared.push(EncodedAsset {
            format: asset.format,
            storage: asset.storage,
            path_encoding,
            path,
        });
    }
    if prepared.is_empty() {
        return Err(StorageError::EmptyAssets);
    }
    Ok(prepared)
}

fn validate_asset_path(storage: AssetStorage, path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        return Err(StorageError::InvalidAssetPath("path is empty".into()));
    }
    if storage == AssetStorage::Managed {
        let mut has_normal_component = false;
        for component in path.components() {
            match component {
                Component::Normal(_) => has_normal_component = true,
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(StorageError::InvalidAssetPath(
                        "managed paths must stay below the library root".into(),
                    ));
                }
            }
        }
        if !has_normal_component || path.to_str().is_none() {
            return Err(StorageError::InvalidAssetPath(
                "managed paths must be portable UTF-8 relative paths".into(),
            ));
        }
    }
    Ok(())
}

fn encode_path(path: &Path) -> (&'static str, Vec<u8>) {
    if let Some(path) = path.to_str() {
        return ("utf8", path.as_bytes().to_vec());
    }

    #[cfg(unix)]
    {
        ("unix", path.as_os_str().as_bytes().to_vec())
    }
    #[cfg(windows)]
    {
        let bytes = path
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect();
        ("windows", bytes)
    }
}

fn decode_path(encoding: &str, bytes: Vec<u8>) -> Result<PathBuf> {
    match encoding {
        "utf8" => String::from_utf8(bytes)
            .map(PathBuf::from)
            .map_err(|_| StorageError::InvalidPathData(encoding.into())),
        "unix" => {
            #[cfg(unix)]
            {
                Ok(PathBuf::from(OsString::from_vec(bytes)))
            }
            #[cfg(not(unix))]
            {
                String::from_utf8(bytes)
                    .map(PathBuf::from)
                    .map_err(|_| StorageError::InvalidPathData("unix".into()))
            }
        }
        "windows" => decode_windows_path(&bytes),
        _ => Err(StorageError::InvalidPathEncoding(encoding.into())),
    }
}

fn decode_windows_path(bytes: &[u8]) -> Result<PathBuf> {
    if !bytes.len().is_multiple_of(2) {
        return Err(StorageError::InvalidPathData("windows".into()));
    }
    let units = bytes
        .chunks_exact(2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>();

    #[cfg(windows)]
    {
        Ok(PathBuf::from(OsString::from_wide(&units)))
    }
    #[cfg(not(windows))]
    {
        String::from_utf16(&units)
            .map(PathBuf::from)
            .map_err(|_| StorageError::InvalidPathData("windows".into()))
    }
}

fn optional_text(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn sortable(value: &str) -> String {
    value.trim().to_lowercase()
}

fn decode_format(value: &str) -> Result<BookFormat> {
    BookFormat::parse(value).ok_or_else(|| StorageError::InvalidAssetFormat(value.into()))
}

fn decode_storage(value: &str) -> Result<AssetStorage> {
    AssetStorage::parse(value).ok_or_else(|| StorageError::InvalidAssetStorage(value.into()))
}

fn decode_health(value: &str) -> Result<AssetHealth> {
    AssetHealth::parse(value).ok_or_else(|| StorageError::InvalidAssetHealth(value.into()))
}

fn inspect_reference_asset(path: &Path) -> AssetHealth {
    match std::fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => AssetHealth::Missing,
        Ok(metadata) if metadata.is_file() => match File::open(path) {
            Ok(_) => AssetHealth::Available,
            Err(_) => AssetHealth::Unreadable,
        },
        Ok(_) | Err(_) => AssetHealth::Unreadable,
    }
}

fn checked_count(count: i64) -> Result<u64> {
    u64::try_from(count).map_err(|_| StorageError::InvalidCount(count))
}

fn current_generation(
    connection: &Connection,
    logical_generation: u64,
) -> Result<LibraryGeneration> {
    let data_version: i64 =
        connection.pragma_query_value(None, "data_version", |row| row.get(0))?;
    Ok(LibraryGeneration {
        connection_changes: logical_generation,
        data_version: checked_count(data_version)?,
    })
}

fn prepare_selection(
    transaction: &Transaction<'_>,
    selection: &BookSelection,
    logical_generation: u64,
) -> Result<(u64, u64)> {
    if let BookSelection::AllMatching { generation, .. } = selection
        && current_generation(transaction, logical_generation)? != *generation
    {
        return Err(StorageError::StaleSelection);
    }

    transaction.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS lectern_selected_books( \
             book_id INTEGER PRIMARY KEY \
         ) WITHOUT ROWID; \
         CREATE TEMP TABLE IF NOT EXISTS lectern_selection_exclusions( \
             book_id INTEGER PRIMARY KEY \
         ) WITHOUT ROWID;",
    )?;
    let mut temporary_changes =
        u64::try_from(transaction.execute("DELETE FROM temp.lectern_selected_books", [])?)
            .expect("SQLite changed-row count fits u64");
    temporary_changes +=
        u64::try_from(transaction.execute("DELETE FROM temp.lectern_selection_exclusions", [])?)
            .expect("SQLite changed-row count fits u64");

    match selection {
        BookSelection::Explicit(books) => {
            let mut insert = transaction.prepare_cached(
                "INSERT OR IGNORE INTO temp.lectern_selected_books(book_id) \
                 SELECT id FROM books WHERE id = ?1",
            )?;
            for id in books {
                temporary_changes += u64::try_from(insert.execute([id.value()])?)
                    .expect("SQLite changed-row count fits u64");
            }
        }
        BookSelection::AllMatching {
            query, excluded, ..
        } => {
            let mut insert_exclusion = transaction.prepare_cached(
                "INSERT OR IGNORE INTO temp.lectern_selection_exclusions(book_id) VALUES (?1)",
            )?;
            for id in excluded {
                temporary_changes += u64::try_from(insert_exclusion.execute([id.value()])?)
                    .expect("SQLite changed-row count fits u64");
            }
            drop(insert_exclusion);

            let plan = LibraryQueryPlan::new(query)?;
            let exclusion = "NOT EXISTS ( \
                SELECT 1 FROM temp.lectern_selection_exclusions excluded \
                WHERE excluded.book_id = b.id \
            )";
            let where_clause = if plan.where_clause.is_empty() {
                format!("WHERE {exclusion}")
            } else {
                format!("{} AND {exclusion}", plan.where_clause)
            };
            let sql = format!(
                "INSERT OR IGNORE INTO temp.lectern_selected_books(book_id) \
                 SELECT b.id FROM books b {} {where_clause}",
                plan.joins,
            );
            temporary_changes += u64::try_from(
                transaction
                    .prepare_cached(&sql)?
                    .execute(rusqlite::params_from_iter(plan.bindings))?,
            )
            .expect("SQLite changed-row count fits u64");
        }
    }

    let count: i64 = transaction.query_row(
        "SELECT count(*) FROM temp.lectern_selected_books",
        [],
        |row| row.get(0),
    )?;
    Ok((checked_count(count)?, temporary_changes))
}

fn prepare_bulk_tag_tables(transaction: &Transaction<'_>) -> Result<u64> {
    transaction.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS lectern_bulk_add_tags( \
             tag_id INTEGER PRIMARY KEY \
         ) WITHOUT ROWID; \
         CREATE TEMP TABLE IF NOT EXISTS lectern_bulk_remove_tags( \
             tag_id INTEGER PRIMARY KEY \
         ) WITHOUT ROWID;",
    )?;
    let cleared_add = transaction.execute("DELETE FROM temp.lectern_bulk_add_tags", [])?;
    let cleared_remove = transaction.execute("DELETE FROM temp.lectern_bulk_remove_tags", [])?;
    Ok(u64::try_from(cleared_add + cleared_remove).expect("SQLite changed-row count fits u64"))
}

const BULK_REMOVAL_TRIGGERS: [&str; 3] = [
    "book_covers_after_delete_summary",
    "book_assets_after_delete_summary",
    "books_after_delete",
];

fn suspend_bulk_removal_triggers(transaction: &Transaction<'_>) -> Result<[String; 3]> {
    let definitions = BULK_REMOVAL_TRIGGERS
        .map(|name| {
            transaction
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE type = 'trigger' AND name = ?1",
                    [name],
                    |row| row.get::<_, String>(0),
                )
                .map_err(StorageError::from)
        })
        .into_iter()
        .collect::<Result<Vec<_>>>()?
        .try_into()
        .expect("three trigger definitions produce a three-element array");
    transaction.execute_batch(
        "DROP TRIGGER book_covers_after_delete_summary; \
         DROP TRIGGER book_assets_after_delete_summary; \
         DROP TRIGGER books_after_delete;",
    )?;
    Ok(definitions)
}

fn restore_bulk_removal_triggers(
    transaction: &Transaction<'_>,
    definitions: &[String; 3],
) -> Result<()> {
    for definition in definitions {
        transaction.execute_batch(definition)?;
    }
    Ok(())
}

fn resolve_bulk_tag(
    transaction: &Transaction<'_>,
    reference: &TagReference,
) -> Result<(TagId, bool)> {
    match reference {
        TagReference::Existing(id) => {
            let exists = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM tags WHERE id = ?1)",
                [id.value()],
                |row| row.get::<_, bool>(0),
            )?;
            if exists {
                Ok((*id, false))
            } else {
                Err(StorageError::InvalidCuration(format!(
                    "tag {id} does not exist"
                )))
            }
        }
        TagReference::New(name) => resolve_new_bulk_tag(transaction, name, TagColor::default()),
        TagReference::NewColored { name, color } => resolve_new_bulk_tag(transaction, name, *color),
    }
}

fn resolve_new_bulk_tag(
    transaction: &Transaction<'_>,
    name: &str,
    color: TagColor,
) -> Result<(TagId, bool)> {
    let name = normalize_name(NameKind::Tag, name)
        .map_err(|error| StorageError::InvalidCuration(error.to_string()))?;
    let key = identity_key(&name);
    if let Some(id) = transaction
        .query_row(
            "SELECT id FROM tags WHERE identity_key = ?1",
            [&key],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
    {
        return Ok((TagId::new(id), false));
    }
    transaction.execute(
        "INSERT INTO tags(name, identity_key, color) VALUES (?1, ?2, ?3)",
        params![name, key, color.as_str()],
    )?;
    Ok((TagId::new(transaction.last_insert_rowid()), true))
}

fn rebuild_selected_tag_projections(transaction: &Transaction<'_>) -> Result<()> {
    transaction.execute(
        "UPDATE books AS b SET tags_search = coalesce(( \
             SELECT group_concat(name, ' ') FROM ( \
                 SELECT t.name AS name FROM book_tags bt \
                 JOIN tags t ON t.id = bt.tag_id \
                 WHERE bt.book_id = b.id \
                 ORDER BY t.identity_key, t.id \
             ) \
         ), '') \
         WHERE b.id IN (SELECT book_id FROM temp.lectern_selected_books)",
        [],
    )?;
    Ok(())
}

struct LibraryQueryPlan {
    bindings: Vec<rusqlite::types::Value>,
    joins: String,
    where_clause: String,
    order: &'static str,
    has_full_text_search: bool,
}

impl LibraryQueryPlan {
    fn new(query: &LibraryQuery) -> Result<Self> {
        let expression = SearchExpression::parse(&query.search)?;
        let mut bindings = Vec::new();
        let mut predicates = Vec::new();
        let mut joins = Vec::new();
        let mut full_text_clauses = Vec::new();

        add_structured_search(
            &expression,
            &mut bindings,
            &mut predicates,
            &mut full_text_clauses,
        );

        let has_full_text_search = !full_text_clauses.is_empty();
        if has_full_text_search {
            bindings.push(full_text_clauses.join(" AND ").into());
            predicates.push(format!("books_fts MATCH ?{}", bindings.len()));
            joins.push("JOIN books_fts ON books_fts.rowid = b.id".to_owned());
        }

        for facet in &query.facets.contributors {
            bindings.push(facet.contributor.value().into());
            let contributor = bindings.len();
            let role = if facet.author_only {
                " AND bc.role = 'author'"
            } else {
                ""
            };
            predicates.push(format!(
                "b.id IN (SELECT bc.book_id FROM book_contributors bc \
                 WHERE bc.contributor_id = ?{contributor}{role})"
            ));
        }
        if let Some(series) = query.facets.series {
            bindings.push(series.value().into());
            predicates.push(format!(
                "b.id IN (SELECT sm.book_id FROM series_memberships sm \
                 WHERE sm.series_id = ?{})",
                bindings.len()
            ));
        }
        for tag in &query.facets.included_tags {
            bindings.push(tag.value().into());
            predicates.push(format!(
                "b.id IN (SELECT bt.book_id FROM book_tags bt WHERE bt.tag_id = ?{})",
                bindings.len()
            ));
        }
        for tag in &query.facets.excluded_tags {
            bindings.push(tag.value().into());
            predicates.push(format!(
                "b.id NOT IN (SELECT bt.book_id FROM book_tags bt WHERE bt.tag_id = ?{})",
                bindings.len()
            ));
        }

        if let Some(format) = query.format {
            push_asset_exists(&mut bindings, &mut predicates, "format", format.as_str());
        }

        if let Some(health) = query.asset_health {
            push_asset_exists(&mut bindings, &mut predicates, "health", health.as_str());
        }

        let where_clause = if predicates.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", predicates.join(" AND "))
        };
        let order = match query.sort {
            SortOrder::Title => "b.sort_title, b.id",
            SortOrder::Author => "b.sort_authors = '', b.sort_authors, b.sort_title, b.id",
            SortOrder::RecentlyAdded => "b.added_at DESC, b.id DESC",
            SortOrder::Series => {
                "b.series_key IS NULL, b.series_key, b.series_index IS NULL, \
                 b.series_index, b.sort_title, b.id"
            }
        };
        Ok(Self {
            bindings,
            joins: joins.join(" "),
            where_clause,
            order,
            has_full_text_search,
        })
    }
}

fn add_structured_search(
    expression: &SearchExpression,
    bindings: &mut Vec<rusqlite::types::Value>,
    predicates: &mut Vec<String>,
    full_text_clauses: &mut Vec<String>,
) {
    for clause in expression.clauses() {
        match clause {
            SearchClause::Any(value) => full_text_clauses.push(fts_match(None, value)),
            SearchClause::Title(value) => {
                full_text_clauses.push(fts_match(Some("title"), value));
            }
            SearchClause::Author(value) => {
                full_text_clauses.push(fts_match(Some("authors_search"), value));
            }
            SearchClause::Contributor(value) => {
                full_text_clauses.push(fts_match(Some("contributors_search"), value));
            }
            SearchClause::Series(value) => {
                full_text_clauses.push(fts_match(Some("series"), value));
            }
            SearchClause::Tag(value) => {
                full_text_clauses.push(fts_match(Some("tags_search"), value));
            }
            SearchClause::Publisher(value) => {
                full_text_clauses.push(fts_match(Some("publisher"), value));
            }
            SearchClause::Language(language) => {
                bindings.push(language.clone().into());
                predicates.push(format!("lower(b.language) = ?{}", bindings.len()));
            }
            SearchClause::Format(format) => {
                push_asset_exists(bindings, predicates, "format", format.as_str());
            }
            SearchClause::File(health) => {
                push_asset_exists(bindings, predicates, "health", health.as_str());
            }
        }
    }
}

fn fts_match(column: Option<&str>, value: &TextMatch) -> String {
    let (value, prefix) = match value {
        TextMatch::Prefix(value) => (value, "*"),
        TextMatch::Phrase(value) => (value, ""),
    };
    let value = value.replace('"', "\"\"");
    column.map_or_else(
        || format!("\"{value}\"{prefix}"),
        |column| format!("{column} : \"{value}\"{prefix}"),
    )
}

fn push_asset_exists(
    bindings: &mut Vec<rusqlite::types::Value>,
    predicates: &mut Vec<String>,
    column: &'static str,
    value: &str,
) {
    bindings.push(value.to_owned().into());
    predicates.push(format!(
        "EXISTS (SELECT 1 FROM book_assets ba \
         WHERE ba.book_id = b.id AND ba.{column} = ?{})",
        bindings.len()
    ));
}

fn book_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<BookSummary> {
    Ok(BookSummary {
        id: BookId::new(row.get(0)?),
        title: row.get(1)?,
        authors: row.get(2)?,
        series: row.get(3)?,
        series_index: organisation::series_index_from_database(row.get(4)?, 4)?,
        has_cover: row.get(5)?,
        has_file_issue: row.get(6)?,
    })
}

fn query_window_with_plan(
    connection: &Connection,
    plan: &LibraryQueryPlan,
    offset: i64,
    limit: u32,
) -> Result<Vec<BookSummary>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT b.id, b.title, b.authors, b.series, b.series_index, \
         b.has_cover, b.has_file_issue \
         FROM books b {} {} ORDER BY {} LIMIT ?{} OFFSET ?{}",
        plan.joins,
        plan.where_clause,
        plan.order,
        plan.bindings.len() + 1,
        plan.bindings.len() + 2,
    );
    let mut bindings = plan.bindings.clone();
    bindings.push(i64::from(limit).into());
    bindings.push(offset.into());
    let mut statement = connection.prepare_cached(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(bindings), book_summary)?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn query_first_page_with_total(
    connection: &Connection,
    plan: &LibraryQueryPlan,
    limit: u32,
) -> Result<(u64, Vec<BookSummary>)> {
    let sql = format!(
        "SELECT b.id, b.title, b.authors, b.series, b.series_index, \
         b.has_cover, b.has_file_issue, count(*) OVER() \
         FROM books b {} {} ORDER BY {} LIMIT ?{}",
        plan.joins,
        plan.where_clause,
        plan.order,
        plan.bindings.len() + 1,
    );
    let mut bindings = plan.bindings.clone();
    bindings.push(i64::from(limit).into());
    let mut statement = connection.prepare_cached(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(bindings), |row| {
        Ok((book_summary(row)?, row.get::<_, i64>(7)?))
    })?;
    let mut books = Vec::new();
    let mut total = 0;
    for row in rows {
        let (book, count) = row?;
        if books.is_empty() {
            total = u64::try_from(count).map_err(|_| StorageError::InvalidCount(count))?;
        }
        books.push(book);
    }
    Ok((total, books))
}
#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    #[cfg(unix)]
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    use lectern_core::{
        AssetHealth, AssetHealthReport, AssetId, AssetStorage, Book, BookAssetDraft, BookFormat,
        BookId, BookMetadataDraft, LibraryQuery, ReimportMetadataPolicy, SortOrder,
        organisation::{
            BookEdit, BookIdentifierEdit, BookSelection, BulkTagEdit, ContributorCreditEdit,
            ContributorFacet, ContributorReference, ContributorRole, ExactFacets,
            IdentifierTypeReference, ImportedContributorCredit, ImportedOrganisation,
            SavedSearchId, SearchExpression, SeriesIndex, SeriesMembershipEdit, SeriesReference,
            TagId, TagReference, VocabularyMutationResult,
        },
    };
    use rusqlite::{Connection, params};

    use super::{
        BookImport, ImportRecord, LibraryDatabase, SCHEMA, SCHEMA_VERSION, StorageError,
        configure_persistent_database, decode_path, initialize_schema_transaction,
    };

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDatabase(PathBuf);

    impl TestDatabase {
        fn new(label: &str) -> Self {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!(
                "lectern-storage-{label}-{}-{id}.sqlite3",
                std::process::id()
            )))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDatabase {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let path = PathBuf::from(format!("{}{}", self.0.display(), suffix));
                if path.exists() {
                    fs::remove_file(path).expect("remove test database file");
                }
            }
        }
    }

    struct TestAsset(PathBuf);

    impl TestAsset {
        fn file(label: &str) -> Self {
            let path = temporary_asset_path(label);
            fs::write(&path, b"publication").expect("write test asset");
            Self(path)
        }

        fn directory(label: &str) -> Self {
            let path = temporary_asset_path(label);
            fs::create_dir(&path).expect("create test asset directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestAsset {
        fn drop(&mut self) {
            if self.0.is_dir() {
                fs::remove_dir(&self.0).expect("remove test asset directory");
            } else if self.0.exists() {
                fs::remove_file(&self.0).expect("remove test asset file");
            }
        }
    }

    fn temporary_asset_path(label: &str) -> PathBuf {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "lectern-storage-{label}-{}-{id}.epub",
            std::process::id()
        ))
    }

    fn metadata(title: &str, authors: &str) -> BookMetadataDraft {
        BookMetadataDraft {
            title: title.into(),
            authors: authors.into(),
            series: None,
            publisher: None,
            language: Some("en".into()),
            description: None,
            imported_organisation: None,
        }
    }

    fn asset(path: impl Into<PathBuf>, format: BookFormat) -> BookAssetDraft {
        BookAssetDraft {
            format,
            storage: AssetStorage::Reference,
            path: path.into(),
        }
    }

    fn record(path: &str, title: &str, authors: &str) -> BookImport {
        record_with_format(path, title, authors, BookFormat::Epub)
    }

    fn record_with_format(
        path: &str,
        title: &str,
        authors: &str,
        format: BookFormat,
    ) -> BookImport {
        BookImport {
            book: metadata(title, authors),
            assets: vec![asset(path, format)],
            cover_thumbnail: None,
        }
    }

    fn schema_without_book_summary_state() -> String {
        SCHEMA
            .replace(
                "    has_cover    INTEGER NOT NULL DEFAULT 0 CHECK (has_cover IN (0, 1)),\n",
                "",
            )
            .replace(
                "    has_file_issue INTEGER NOT NULL DEFAULT 0 CHECK (has_file_issue IN (0, 1)),\n",
                "",
            )
            .replace(
                r"CREATE TRIGGER book_covers_after_insert_summary
AFTER INSERT ON book_covers BEGIN
    UPDATE books SET has_cover = 1 WHERE id = new.book_id;
END;

CREATE TRIGGER book_covers_after_delete_summary
AFTER DELETE ON book_covers BEGIN
    UPDATE books SET has_cover = 0 WHERE id = old.book_id;
END;

CREATE TRIGGER book_assets_after_insert_summary
AFTER INSERT ON book_assets
WHEN new.health IN ('missing', 'unreadable')
BEGIN
    UPDATE books SET has_file_issue = 1 WHERE id = new.book_id;
END;

CREATE TRIGGER book_assets_after_health_update_summary
AFTER UPDATE OF health ON book_assets
WHEN old.health IS NOT new.health
BEGIN
    UPDATE books
    SET has_file_issue = EXISTS(
        SELECT 1 FROM book_assets
        WHERE book_id = new.book_id AND health IN ('missing', 'unreadable')
    )
    WHERE id = new.book_id;
END;

CREATE TRIGGER book_assets_after_delete_summary
AFTER DELETE ON book_assets
BEGIN
    UPDATE books
    SET has_file_issue = EXISTS(
        SELECT 1 FROM book_assets
        WHERE book_id = old.book_id AND health IN ('missing', 'unreadable')
    )
    WHERE id = old.book_id;
END;

",
                "",
            )
    }

    fn create_legacy_library(path: &Path, version: i64, format: BookFormat) {
        let allowed_formats = if version == 1 {
            "CHECK (format IN ('epub'))"
        } else {
            "CHECK (format IN ('epub', 'pdf'))"
        };
        let schema = format!(
            r"
            CREATE TABLE books (
                id INTEGER PRIMARY KEY,
                title TEXT NOT NULL,
                sort_title TEXT NOT NULL,
                authors TEXT NOT NULL,
                sort_authors TEXT NOT NULL,
                series TEXT,
                publisher TEXT,
                language TEXT,
                description TEXT,
                format TEXT NOT NULL {allowed_formats},
                source_path TEXT NOT NULL UNIQUE,
                added_at INTEGER NOT NULL DEFAULT (unixepoch()),
                modified_at INTEGER NOT NULL DEFAULT (unixepoch())
            );
            CREATE INDEX books_sort_title_idx ON books(sort_title, id);
            CREATE INDEX books_sort_authors_idx ON books(sort_authors, sort_title, id);
            CREATE INDEX books_added_at_idx ON books(added_at DESC, id DESC);
            CREATE INDEX books_format_title_idx ON books(format, sort_title, id);
            CREATE TABLE book_covers (
                book_id INTEGER PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
                jpeg BLOB NOT NULL
            );
            CREATE VIRTUAL TABLE books_fts USING fts5(
                title, authors, series, publisher,
                content='books', content_rowid='id',
                tokenize='unicode61 remove_diacritics 2', prefix='2 3'
            );
            CREATE TRIGGER books_after_insert AFTER INSERT ON books BEGIN
                INSERT INTO books_fts(rowid, title, authors, series, publisher)
                VALUES (new.id, new.title, new.authors, new.series, new.publisher);
            END;
            CREATE TRIGGER books_after_delete AFTER DELETE ON books BEGIN
                INSERT INTO books_fts(books_fts, rowid, title, authors, series, publisher)
                VALUES ('delete', old.id, old.title, old.authors, old.series, old.publisher);
            END;
            CREATE TRIGGER books_after_update AFTER UPDATE ON books BEGIN
                INSERT INTO books_fts(books_fts, rowid, title, authors, series, publisher)
                VALUES ('delete', old.id, old.title, old.authors, old.series, old.publisher);
                INSERT INTO books_fts(rowid, title, authors, series, publisher)
                VALUES (new.id, new.title, new.authors, new.series, new.publisher);
            END;
            PRAGMA user_version = {version};
            "
        );
        let connection = Connection::open(path).expect("open legacy database");
        connection
            .execute_batch(&schema)
            .expect("create legacy schema");
        connection
            .execute(
                "INSERT INTO books (
                    id, title, sort_title, authors, sort_authors, format, source_path,
                    added_at, modified_at
                 ) VALUES (7, 'Dune', 'dune', 'Frank Herbert', 'frank herbert', ?1,
                           '/books/dune.epub', 100, 200)",
                [format.as_str()],
            )
            .expect("insert legacy book");
        connection
            .execute(
                "INSERT INTO book_covers(book_id, jpeg) VALUES (7, x'010203')",
                [],
            )
            .expect("insert legacy cover");
    }

    #[test]
    fn stores_one_logical_book_with_multiple_formats() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let aggregate = BookImport {
            book: metadata("Dune", "Frank Herbert"),
            assets: vec![
                asset("/books/dune.epub", BookFormat::Epub),
                asset("/books/dune.pdf", BookFormat::Pdf),
            ],
            cover_thumbnail: None,
        };
        let id = database.import_books(&[aggregate]).expect("import book")[0];

        assert_eq!(database.count().expect("count books"), 1);
        for format in BookFormat::ALL {
            let results = database
                .query(&LibraryQuery {
                    format: Some(format),
                    ..LibraryQuery::default()
                })
                .expect("filter format");
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].id, id);
        }
        let book = database
            .get_book(id)
            .expect("load book")
            .expect("book exists");
        assert_eq!(book.assets.len(), 2);
        assert_eq!(book.assets[0].format, BookFormat::Epub);
        assert_eq!(book.assets[1].format, BookFormat::Pdf);
    }

    #[test]
    fn paged_queries_preserve_full_projection_order_and_count() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        database
            .import_books(&[
                record("/books/dune.epub", "Dune", "Frank Herbert"),
                record("/books/dune-messiah.epub", "Dune Messiah", "Frank Herbert"),
                record_with_format(
                    "/books/foundation.pdf",
                    "Foundation",
                    "Isaac Asimov",
                    BookFormat::Pdf,
                ),
                record(
                    "/books/game.epub",
                    "A Game of Thrones",
                    "George R. R. Martin",
                ),
                record("/books/hobbit.epub", "The Hobbit", "J. R. R. Tolkien"),
            ])
            .expect("import books");

        let queries = [
            LibraryQuery::default(),
            LibraryQuery {
                sort: SortOrder::Author,
                ..LibraryQuery::default()
            },
            LibraryQuery {
                sort: SortOrder::RecentlyAdded,
                ..LibraryQuery::default()
            },
            LibraryQuery {
                search: "Dun".into(),
                ..LibraryQuery::default()
            },
            LibraryQuery {
                format: Some(BookFormat::Pdf),
                ..LibraryQuery::default()
            },
            LibraryQuery {
                asset_health: Some(AssetHealth::Unknown),
                ..LibraryQuery::default()
            },
        ];

        for query in queries {
            let full = database.query(&query).expect("full query");
            assert_eq!(
                database.query_window(&query, 2, 2).expect("windowed query"),
                full.iter().skip(2).take(2).cloned().collect::<Vec<_>>()
            );
            let mut offset = 0;
            let mut paged = Vec::new();
            loop {
                let page = database.query_page(&query, offset, 2).expect("paged query");
                assert_eq!(
                    page.total,
                    u64::try_from(full.len()).expect("count fits u64")
                );
                assert_eq!(page.offset, offset);
                offset += u64::try_from(page.books.len()).expect("page length fits u64");
                let complete = page.books.len() < 2;
                paged.extend(page.books);
                if complete {
                    break;
                }
            }
            assert_eq!(paged, full);
        }

        let empty = database
            .query_page(&LibraryQuery::default(), 0, 0)
            .expect("zero-sized page");
        assert_eq!(empty.total, 5);
        assert!(empty.books.is_empty());
        assert!(matches!(
            database.query_page(&LibraryQuery::default(), u64::MAX, 1),
            Err(StorageError::InvalidPageOffset(u64::MAX))
        ));
    }

    #[test]
    fn rescans_references_without_rewriting_unchanged_asset_states() {
        let present = TestAsset::file("present");
        let unreadable = TestAsset::directory("directory");
        let missing = temporary_asset_path("missing");
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        database
            .import_books(&[
                record(
                    present.path().to_string_lossy().as_ref(),
                    "Present",
                    "Author",
                ),
                record(missing.to_string_lossy().as_ref(), "Missing", "Author"),
                record(
                    unreadable.path().to_string_lossy().as_ref(),
                    "Unreadable",
                    "Author",
                ),
            ])
            .expect("import records");

        let report = database.rescan_reference_assets().expect("scan references");
        assert_eq!(
            report,
            AssetHealthReport {
                checked: 3,
                available: 1,
                missing: 1,
                unreadable: 1,
                changed: 3,
            }
        );
        assert_eq!(
            database
                .query(&LibraryQuery {
                    asset_health: Some(AssetHealth::Missing),
                    ..LibraryQuery::default()
                })
                .expect("query missing books")
                .len(),
            1
        );
        assert_eq!(
            database
                .query(&LibraryQuery {
                    asset_health: Some(AssetHealth::Unreadable),
                    ..LibraryQuery::default()
                })
                .expect("query unreadable books")
                .len(),
            1
        );
        assert!(
            database
                .query(&LibraryQuery::default())
                .expect("query books")
                .iter()
                .any(|book| book.has_file_issue)
        );

        let repeat = database
            .rescan_reference_assets()
            .expect("repeat scan references");
        assert_eq!(repeat.changed, 0);

        fs::remove_file(present.path()).expect("remove present asset");
        let changed = database
            .rescan_reference_assets()
            .expect("scan changed references");
        assert_eq!(changed.missing, 2);
        assert_eq!(changed.changed, 1);
    }

    #[test]
    fn relinking_preserves_asset_identity_book_metadata_and_cover() {
        let replacement = TestAsset::file("replacement");
        let missing = temporary_asset_path("relink-missing");
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let mut imported = record(missing.to_string_lossy().as_ref(), "Dune", "Frank Herbert");
        imported.book.series = Some("Dune Chronicles".into());
        imported.cover_thumbnail = Some(vec![1, 2, 3]);
        let book_id = database.import_books(&[imported]).expect("import book")[0];
        let original = database
            .get_book(book_id)
            .expect("load original book")
            .expect("original book exists");
        let asset_id = original.assets[0].id;

        database
            .rescan_reference_assets()
            .expect("scan missing asset");
        database
            .relink_reference_asset(asset_id, replacement.path(), BookFormat::Epub)
            .expect("relink asset");

        let relinked = database
            .get_book(book_id)
            .expect("load relinked book")
            .expect("relinked book exists");
        assert_eq!(relinked.id, book_id);
        assert_eq!(relinked.title, original.title);
        assert_eq!(relinked.authors, original.authors);
        assert_eq!(relinked.series, original.series);
        assert_eq!(relinked.assets[0].id, asset_id);
        assert_eq!(relinked.assets[0].path, replacement.path());
        assert_eq!(relinked.assets[0].health, AssetHealth::Available);
        assert_eq!(
            database.load_cover(book_id).expect("load preserved cover"),
            Some(vec![1, 2, 3])
        );
    }

    #[test]
    fn relinking_rejects_a_wrong_format_or_existing_reference_path() {
        let replacement = TestAsset::file("owned-replacement");
        let missing = temporary_asset_path("relink-conflict");
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let owner_id = database
            .import_books(&[record(
                replacement.path().to_string_lossy().as_ref(),
                "Owned",
                "Author",
            )])
            .expect("import owner")[0];
        let relinked_id = database
            .import_books(&[record(
                missing.to_string_lossy().as_ref(),
                "Missing",
                "Author",
            )])
            .expect("import missing book")[0];
        let relinked_asset = database
            .get_book(relinked_id)
            .expect("load missing book")
            .expect("missing book exists")
            .assets[0]
            .id;
        let owner_asset = database
            .get_book(owner_id)
            .expect("load owner book")
            .expect("owner book exists")
            .assets[0]
            .id;

        assert!(matches!(
            database.relink_reference_asset(relinked_asset, replacement.path(), BookFormat::Pdf),
            Err(StorageError::RelinkFormatMismatch {
                expected: BookFormat::Epub,
                found: BookFormat::Pdf,
            })
        ));
        assert!(matches!(
            database.relink_reference_asset(relinked_asset, replacement.path(), BookFormat::Epub),
            Err(StorageError::ReferencePathInUse(id)) if id == owner_asset
        ));
        assert!(matches!(
            database.replace_reference_asset(
                relinked_asset,
                replacement.path(),
                BookFormat::Epub,
            ),
            Err(StorageError::ReferencePathInUse(id)) if id == owner_asset
        ));
    }

    #[test]
    fn relinking_rejects_a_healthy_asset_but_replacement_preserves_identity_and_bytes() {
        let original_source = TestAsset::file("replace-original");
        let replacement_source = TestAsset::file("replace-new");
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let mut imported = record(
            original_source.path().to_string_lossy().as_ref(),
            "Dune",
            "Frank Herbert",
        );
        imported.book.series = Some("Dune Chronicles".into());
        imported.cover_thumbnail = Some(vec![7, 8, 9]);
        let book_id = database.import_books(&[imported]).expect("import book")[0];
        database
            .rescan_reference_assets()
            .expect("scan healthy asset");
        let original = database
            .get_book(book_id)
            .expect("load original")
            .expect("book exists");
        let asset_id = original.assets[0].id;

        assert!(matches!(
            database.relink_reference_asset(
                asset_id,
                replacement_source.path(),
                BookFormat::Epub,
            ),
            Err(StorageError::RelinkAssetAvailable(id)) if id == asset_id
        ));
        assert!(matches!(
            database.replace_reference_asset(asset_id, replacement_source.path(), BookFormat::Pdf,),
            Err(StorageError::ReplacementFormatMismatch {
                expected: BookFormat::Epub,
                found: BookFormat::Pdf,
            })
        ));
        database
            .replace_reference_asset(asset_id, replacement_source.path(), BookFormat::Epub)
            .expect("replace healthy asset");

        let replaced = database
            .get_book(book_id)
            .expect("load replaced book")
            .expect("book remains");
        assert_eq!(replaced.id, original.id);
        assert_eq!(replaced.title, original.title);
        assert_eq!(replaced.authors, original.authors);
        assert_eq!(replaced.series, original.series);
        assert_eq!(replaced.assets[0].id, asset_id);
        assert_eq!(replaced.assets[0].path, replacement_source.path());
        assert_eq!(replaced.assets[0].health, AssetHealth::Available);
        assert_eq!(
            database.load_cover(book_id).expect("load cover"),
            Some(vec![7, 8, 9])
        );
        assert_eq!(
            fs::read(original_source.path()).expect("read original source"),
            b"publication"
        );
        assert_eq!(
            fs::read(replacement_source.path()).expect("read replacement source"),
            b"publication"
        );
    }

    #[test]
    fn persistent_libraries_require_durable_journaling_and_full_sync() {
        let database_file = TestDatabase::new("durability");
        let database = LibraryDatabase::open(database_file.path()).expect("open library");
        let journal_mode = database
            .connection
            .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
            .expect("read journal mode");
        let synchronous = database
            .connection
            .pragma_query_value(None, "synchronous", |row| row.get::<_, i64>(0))
            .expect("read synchronization mode");

        assert!(
            journal_mode.eq_ignore_ascii_case("wal") || journal_mode.eq_ignore_ascii_case("delete")
        );
        assert_eq!(synchronous, 2);

        let memory = Connection::open_in_memory().expect("open memory database");
        assert!(matches!(
            configure_persistent_database(&memory),
            Err(StorageError::Integrity(message)) if message.contains("rollback journaling")
        ));
    }

    #[test]
    fn migrates_version_one_and_two_libraries_directly() {
        for (version, format) in [(1, BookFormat::Epub), (2, BookFormat::Pdf)] {
            let database_file = TestDatabase::new(&format!("migration-v{version}"));
            create_legacy_library(database_file.path(), version, format);

            let database = LibraryDatabase::open(database_file.path()).expect("migrate database");

            let migrated_version: i64 = database
                .connection
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .expect("read schema version");
            assert_eq!(migrated_version, SCHEMA_VERSION);
            assert_eq!(database.count().expect("count migrated books"), 1);
            let book = database
                .get_book(BookId::new(7))
                .expect("load migrated book")
                .expect("migrated book exists");
            assert_eq!(book.id, BookId::new(7));
            assert_eq!(book.assets.len(), 1);
            assert_eq!(book.assets[0].id.value(), 7);
            assert_eq!(book.assets[0].format, format);
            assert_eq!(book.assets[0].storage, AssetStorage::Reference);
            assert_eq!(book.assets[0].path, PathBuf::from("/books/dune.epub"));
            assert_eq!(
                database.load_cover(book.id).expect("load migrated cover"),
                Some(vec![1, 2, 3])
            );
            assert!(
                database
                    .query(&LibraryQuery::default())
                    .expect("query migrated summary")[0]
                    .has_cover
            );
            assert_eq!(
                database
                    .query(&LibraryQuery {
                        search: "Dun".into(),
                        ..LibraryQuery::default()
                    })
                    .expect("search migrated library")
                    .len(),
                1
            );
            assert!(
                !database
                    .connection
                    .prepare("PRAGMA foreign_key_check")
                    .expect("prepare foreign-key check")
                    .exists([])
                    .expect("run foreign-key check")
            );
        }
    }

    #[test]
    fn migrates_version_eight_with_stored_default_identifier_types() {
        let mut connection = Connection::open_in_memory().expect("open database");
        connection.execute_batch(SCHEMA).expect("create v5 schema");
        {
            let transaction = connection.transaction().expect("begin v8 fixture");
            super::organisation::migrate_v5_to_v6(&transaction).expect("migrate to v6");
            super::organisation::migrate_v6_to_v7(&transaction).expect("migrate to v7");
            super::organisation::migrate_v7_to_v8(&transaction).expect("migrate to v8");
            transaction
                .pragma_update(None, "user_version", 8)
                .expect("mark v8 fixture");
            transaction.commit().expect("commit v8 fixture");
        }

        initialize_schema_transaction(&mut connection).expect("migrate identifier schema");

        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            SCHEMA_VERSION
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM identifier_types", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            10
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT group_concat(name, '|') FROM ( \
                         SELECT name FROM identifier_types ORDER BY id \
                     )",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "ISBN|ASIN|DOI|Google Books|Goodreads|Open Library|OCLC|LCCN|ISSN|arXiv"
        );
    }

    #[test]
    fn migrates_version_three_libraries_with_unknown_asset_health() {
        let database_file = TestDatabase::new("migration-v3");
        let version_three_schema = schema_without_book_summary_state()
            .replace(
                "    health        TEXT NOT NULL DEFAULT 'unknown'\n                  CHECK (health IN ('unknown', 'available', 'missing', 'unreadable')),\n",
                "",
            )
            .replace("CREATE INDEX book_assets_health_book_idx ON book_assets(health, book_id);\n", "");
        let connection = Connection::open(database_file.path()).expect("open v3 database");
        connection
            .execute_batch(&version_three_schema)
            .expect("create v3 schema");
        connection
            .execute(
                "INSERT INTO books (id, title, sort_title, authors, sort_authors) \
                 VALUES (7, 'Dune', 'dune', 'Frank Herbert', 'frank herbert')",
                [],
            )
            .expect("insert v3 book");
        connection
            .execute(
                "INSERT INTO book_assets (id, book_id, format, storage_mode, path_encoding, path) \
                 VALUES (9, 7, 'epub', 'reference', 'utf8', x'2F626F6F6B732F64756E652E65707562')",
                [],
            )
            .expect("insert v3 asset");
        connection
            .pragma_update(None, "user_version", 3)
            .expect("mark v3 schema");
        drop(connection);

        let database = LibraryDatabase::open(database_file.path()).expect("migrate database");
        let book = database
            .get_book(BookId::new(7))
            .expect("load migrated book")
            .expect("migrated book exists");

        assert_eq!(book.assets[0].health, AssetHealth::Unknown);
        let version: i64 = database
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("read migrated version");
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn migrates_version_four_cover_and_health_summaries() {
        let database_file = TestDatabase::new("migration-v4");
        let connection = Connection::open(database_file.path()).expect("open v4 database");
        connection
            .execute_batch(&schema_without_book_summary_state())
            .expect("create v4 schema");
        connection
            .execute(
                "INSERT INTO books (id, title, sort_title, authors, sort_authors) \
                 VALUES (7, 'Dune', 'dune', 'Frank Herbert', 'frank herbert')",
                [],
            )
            .expect("insert v4 book");
        connection
            .execute(
                "INSERT INTO book_assets ( \
                     id, book_id, format, storage_mode, health, path_encoding, path \
                 ) VALUES ( \
                     9, 7, 'epub', 'reference', 'missing', 'utf8', \
                     x'2F626F6F6B732F64756E652E65707562' \
                 )",
                [],
            )
            .expect("insert v4 asset");
        connection
            .execute(
                "INSERT INTO book_covers(book_id, jpeg) VALUES (7, x'010203')",
                [],
            )
            .expect("insert v4 cover");
        connection
            .pragma_update(None, "user_version", 4)
            .expect("mark v4 schema");
        drop(connection);

        let database = LibraryDatabase::open(database_file.path()).expect("migrate database");
        let summary = &database
            .query(&LibraryQuery::default())
            .expect("query migrated summary")[0];

        assert!(summary.has_cover);
        assert!(summary.has_file_issue);
        let version: i64 = database
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("read migrated version");
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn import_preserves_creator_boundaries_and_exact_series_index() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let mut imported = record(
            "/books/earthsea.epub",
            "The Books of Earthsea",
            "Ursula K. Le Guin, Charles Vess",
        );
        imported.book.series = Some("Earthsea".into());
        imported.book.imported_organisation = Some(ImportedOrganisation {
            contributors: vec![
                ImportedContributorCredit {
                    display_name: "Ursula K. Le Guin".into(),
                    role: ContributorRole::Author,
                    position: 0,
                },
                ImportedContributorCredit {
                    display_name: "Charles Vess".into(),
                    role: ContributorRole::Author,
                    position: 1,
                },
            ],
            series_index: Some("1.25".parse::<SeriesIndex>().expect("series index")),
        });

        let id = database
            .import_books(&[imported.clone()])
            .expect("import book")[0];
        let book = database.get_book(id).unwrap().unwrap();
        assert_eq!(book.contributors.len(), 2);
        assert_eq!(
            book.contributors[0].contributor.display_name,
            "Ursula K. Le Guin"
        );
        assert_eq!(
            book.contributors[1].contributor.display_name,
            "Charles Vess"
        );
        assert_eq!(book.authors, "Ursula K. Le Guin, Charles Vess");
        assert_eq!(
            book.series_membership
                .as_ref()
                .and_then(|membership| membership.index)
                .map(|index| index.to_string()),
            Some("1.25".into())
        );

        imported.book.imported_organisation = Some(ImportedOrganisation {
            contributors: vec![ImportedContributorCredit {
                display_name: "Replacement Creator".into(),
                role: ContributorRole::Author,
                position: 0,
            }],
            series_index: Some("9".parse::<SeriesIndex>().expect("series index")),
        });
        assert_eq!(
            database.import_books(&[imported]).expect("re-import book")[0],
            id
        );
        assert_eq!(database.get_book(id).unwrap().unwrap(), book);
    }

    #[test]
    fn reimport_by_known_path_preserves_user_metadata_assets_and_cover() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let original_import = BookImport {
            book: metadata("Dune", "Frank Herbert"),
            assets: vec![
                asset("/books/dune.epub", BookFormat::Epub),
                asset("/books/dune.pdf", BookFormat::Pdf),
            ],
            cover_thumbnail: Some(vec![1, 2, 3]),
        };
        let id = database
            .import_books(std::slice::from_ref(&original_import))
            .expect("first import")[0];
        let mut curated = database
            .get_book(id)
            .expect("load original")
            .expect("original exists");
        curated.title = "Dune: The Desert Planet".into();
        curated.authors = "Frank Herbert; Curated Contributor".into();
        curated.series = Some("The Dune Saga".into());
        curated.publisher = Some("Curated Press".into());
        curated.language = Some("en-AU".into());
        curated.description = Some("A carefully edited library description.".into());
        database.save_book(&curated).expect("save curated metadata");
        let curated = database
            .get_book(id)
            .expect("reload curated metadata")
            .expect("curated book exists");

        let mut reimport = original_import;
        reimport.book = BookMetadataDraft {
            title: "Embedded File Title".into(),
            authors: "Embedded File Author".into(),
            series: Some("Embedded Series".into()),
            publisher: Some("Embedded Publisher".into()),
            language: Some("fr".into()),
            description: Some("Embedded file description.".into()),
            imported_organisation: None,
        };
        reimport.assets.truncate(1);
        reimport.cover_thumbnail = None;

        let replacement_id = database.import_books(&[reimport]).expect("second import")[0];
        let updated = database
            .get_book(id)
            .expect("load updated")
            .expect("updated exists");

        assert_eq!(id, replacement_id);
        assert_eq!(updated, curated);
        assert_eq!(
            database.load_cover(id).expect("load cover"),
            Some(vec![1, 2, 3])
        );
        assert_eq!(
            database
                .query(&LibraryQuery {
                    search: "Desert Planet".into(),
                    ..LibraryQuery::default()
                })
                .expect("search curated title")[0]
                .id,
            id
        );
        assert!(
            database
                .query(&LibraryQuery {
                    search: "Embedded File Title".into(),
                    ..LibraryQuery::default()
                })
                .expect("search incoming title")
                .is_empty()
        );
    }

    #[test]
    fn conflicting_existing_asset_owners_roll_back_batch() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        database
            .import_books(&[
                record("/books/one.epub", "One", "Author One"),
                record_with_format("/books/two.pdf", "Two", "Author Two", BookFormat::Pdf),
            ])
            .expect("seed books");
        let conflict = BookImport {
            book: metadata("Merged", "Nobody"),
            assets: vec![
                asset("/books/one.epub", BookFormat::Epub),
                asset("/books/two.pdf", BookFormat::Pdf),
            ],
            cover_thumbnail: None,
        };

        assert!(matches!(
            database.import_books(&[conflict]),
            Err(StorageError::ConflictingAssetOwners)
        ));
        assert_eq!(database.count().expect("count books"), 2);
        assert!(
            database
                .query(&LibraryQuery {
                    search: "Merged".into(),
                    ..LibraryQuery::default()
                })
                .expect("search rolled-back title")
                .is_empty()
        );
    }

    #[test]
    fn aggregate_validation_is_atomic() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let valid = record("/books/valid.epub", "Valid", "Author");
        let empty = BookImport {
            book: metadata("Empty", "Author"),
            assets: Vec::new(),
            cover_thumbnail: None,
        };

        assert!(matches!(
            database.import_books(&[valid, empty]),
            Err(StorageError::EmptyAssets)
        ));
        assert_eq!(database.count().expect("count books"), 0);
    }

    #[test]
    fn duplicate_format_is_rejected() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let duplicate = BookImport {
            book: metadata("Dune", "Frank Herbert"),
            assets: vec![
                asset("/books/dune.epub", BookFormat::Epub),
                asset("/books/dune-alt.epub", BookFormat::Epub),
            ],
            cover_thumbnail: None,
        };

        assert!(matches!(
            database.import_books(&[duplicate]),
            Err(StorageError::DuplicateAssetFormat(BookFormat::Epub))
        ));
    }

    #[test]
    fn managed_assets_require_safe_portable_relative_paths() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        for path in ["/absolute/book.epub", "../outside.epub"] {
            let imported = BookImport {
                book: metadata("Unsafe", "Author"),
                assets: vec![BookAssetDraft {
                    format: BookFormat::Epub,
                    storage: AssetStorage::Managed,
                    path: path.into(),
                }],
                cover_thumbnail: None,
            };

            assert!(matches!(
                database.import_books(&[imported]),
                Err(StorageError::InvalidAssetPath(_))
            ));
        }
        assert_eq!(database.count().expect("count books"), 0);
    }

    #[test]
    fn shared_managed_location_can_back_distinct_books() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let managed = |title: &str| BookImport {
            book: metadata(title, "Author"),
            assets: vec![BookAssetDraft {
                format: BookFormat::Epub,
                storage: AssetStorage::Managed,
                path: "objects/ab/content.epub".into(),
            }],
            cover_thumbnail: None,
        };

        database
            .import_books(&[managed("One"), managed("Two")])
            .expect("import shared managed content");

        assert_eq!(database.count().expect("count books"), 2);
    }

    #[test]
    fn single_publication_compatibility_import_still_works() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let legacy = ImportRecord {
            book: lectern_core::BookDraft {
                title: "Dune".into(),
                authors: "Frank Herbert".into(),
                series: None,
                publisher: None,
                language: None,
                description: None,
                format: BookFormat::Epub,
                source_path: "/books/dune.epub".into(),
            },
            cover_thumbnail: None,
        };

        let id = database.import_batch(&[legacy]).expect("legacy import")[0];
        let book = database
            .get_book(id)
            .expect("load book")
            .expect("book exists");
        assert_eq!(book.assets.len(), 1);
        assert_eq!(book.assets[0].format, BookFormat::Epub);
    }

    #[test]
    fn metadata_updates_refresh_search_without_changing_assets() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let id = database
            .import_books(&[record("/books/dune.epub", "Dune", "Frank Herbert")])
            .expect("import book")[0];
        let mut book = database
            .get_book(id)
            .expect("load book")
            .expect("book exists");
        let assets = book.assets.clone();
        book.title = "Arrakis".into();

        database.save_book(&book).expect("save metadata");

        assert!(
            database
                .query(&LibraryQuery {
                    search: "Dune".into(),
                    ..LibraryQuery::default()
                })
                .expect("search old title")
                .is_empty()
        );
        assert_eq!(
            database
                .query(&LibraryQuery {
                    search: "Arr".into(),
                    ..LibraryQuery::default()
                })
                .expect("search new title")
                .len(),
            1
        );
        assert_eq!(
            database
                .get_book(id)
                .expect("reload book")
                .expect("book exists")
                .assets,
            assets
        );
    }

    #[test]
    fn attaching_a_reference_format_preserves_the_logical_book_and_source() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let source = TestAsset::file("attach-source");
        let mut imported = record("/books/dune.epub", "Dune", "Frank Herbert");
        imported.book.series = Some("Dune".into());
        imported.cover_thumbnail = Some(vec![1, 2, 3]);
        let id = database.import_books(&[imported]).expect("import book")[0];
        let original = database
            .get_book(id)
            .expect("load original")
            .expect("book exists");

        let attached = database
            .attach_reference_asset(id, BookFormat::Pdf, source.path())
            .expect("attach PDF");

        assert_eq!(database.count().expect("count books"), 1);
        let updated = database
            .get_book(id)
            .expect("load updated")
            .expect("book exists");
        assert_eq!(updated.title, original.title);
        assert_eq!(updated.authors, original.authors);
        assert_eq!(updated.series, original.series);
        assert_eq!(updated.assets.len(), 2);
        assert_eq!(updated.assets[0], original.assets[0]);
        assert_eq!(updated.assets[1].id, attached);
        assert_eq!(updated.assets[1].format, BookFormat::Pdf);
        assert_eq!(updated.assets[1].health, AssetHealth::Available);
        assert_eq!(updated.assets[1].path, source.path());
        assert_eq!(
            database.load_cover(id).expect("load cover"),
            Some(vec![1, 2, 3])
        );
        let filtered = database
            .query(&LibraryQuery {
                format: Some(BookFormat::Pdf),
                ..LibraryQuery::default()
            })
            .expect("filter PDF books");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, id);
        assert_eq!(
            fs::read(source.path()).expect("read attached source"),
            b"publication"
        );
    }

    #[test]
    fn detaching_one_of_two_assets_preserves_book_cover_and_source_bytes() {
        let epub = TestAsset::file("detach-epub");
        let pdf = TestAsset::file("detach-pdf");
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let mut imported = BookImport {
            book: metadata("Dune", "Frank Herbert"),
            assets: vec![
                asset(epub.path(), BookFormat::Epub),
                asset(pdf.path(), BookFormat::Pdf),
            ],
            cover_thumbnail: Some(vec![4, 5, 6]),
        };
        imported.book.series = Some("Dune Chronicles".into());
        let id = database.import_books(&[imported]).expect("import book")[0];
        let original = database
            .get_book(id)
            .expect("load original")
            .expect("book exists");
        let detached = original
            .assets
            .iter()
            .find(|asset| asset.format == BookFormat::Pdf)
            .expect("PDF asset")
            .id;

        assert_eq!(database.detach_asset(detached).expect("detach PDF"), id);

        let updated = database
            .get_book(id)
            .expect("load updated")
            .expect("book remains");
        assert_eq!(updated.title, original.title);
        assert_eq!(updated.authors, original.authors);
        assert_eq!(updated.series, original.series);
        assert_eq!(updated.assets.len(), 1);
        assert_eq!(updated.assets[0].format, BookFormat::Epub);
        assert_eq!(updated.assets[0].id, original.assets[0].id);
        assert_eq!(
            database.load_cover(id).expect("load cover"),
            Some(vec![4, 5, 6])
        );
        assert_eq!(fs::read(epub.path()).expect("read EPUB"), b"publication");
        assert_eq!(fs::read(pdf.path()).expect("read PDF"), b"publication");
    }

    #[test]
    fn detaching_rejects_the_last_asset_and_a_stale_asset_id() {
        let source = TestAsset::file("last-detach");
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let id = database
            .import_books(&[record(
                source.path().to_string_lossy().as_ref(),
                "Dune",
                "Frank Herbert",
            )])
            .expect("import book")[0];
        let asset_id = database
            .get_book(id)
            .expect("load book")
            .expect("book exists")
            .assets[0]
            .id;

        assert!(matches!(
            database.detach_asset(asset_id),
            Err(StorageError::LastAssetDetach { asset, book })
                if asset == asset_id && book == id
        ));
        assert!(matches!(
            database.detach_asset(AssetId::new(i64::MAX)),
            Err(StorageError::AssetNotFound(asset)) if asset == AssetId::new(i64::MAX)
        ));
        assert_eq!(
            database
                .get_book(id)
                .expect("reload book")
                .expect("book remains")
                .assets
                .len(),
            1
        );
        assert_eq!(
            fs::read(source.path()).expect("read source"),
            b"publication"
        );
    }

    #[test]
    fn attaching_a_duplicate_format_is_rejected_without_changing_assets() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let source = TestAsset::file("duplicate-attachment");
        let id = database
            .import_books(&[record("/books/dune.epub", "Dune", "Frank Herbert")])
            .expect("import book")[0];

        assert!(matches!(
            database.attach_reference_asset(id, BookFormat::Epub, source.path()),
            Err(StorageError::BookAlreadyHasFormat {
                book,
                format: BookFormat::Epub,
            }) if book == id
        ));
        assert_eq!(
            database
                .get_book(id)
                .expect("load book")
                .expect("book exists")
                .assets
                .len(),
            1
        );
    }

    #[test]
    fn attaching_a_path_owned_by_another_book_is_rejected() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let source = TestAsset::file("owned-attachment");
        let owner = database
            .import_books(&[record(
                source.path().to_str().expect("UTF-8 test path"),
                "Owner",
                "Author",
            )])
            .expect("import owner")[0];
        let target = database
            .import_books(&[record_with_format(
                "/books/target.pdf",
                "Target",
                "Author",
                BookFormat::Pdf,
            )])
            .expect("import target")[0];
        let owner_asset = database
            .get_book(owner)
            .expect("load owner")
            .expect("owner exists")
            .assets[0]
            .id;

        assert!(matches!(
            database.attach_reference_asset(target, BookFormat::Epub, source.path()),
            Err(StorageError::ReferencePathInUse(asset)) if asset == owner_asset
        ));
        assert_eq!(
            database
                .get_book(target)
                .expect("load target")
                .expect("target exists")
                .assets
                .len(),
            1
        );
    }

    #[test]
    fn attaching_requires_an_existing_book_and_available_file() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let source = TestAsset::file("missing-book-attachment");
        assert!(matches!(
            database.attach_reference_asset(BookId::new(404), BookFormat::Pdf, source.path()),
            Err(StorageError::BookNotFound(book)) if book == BookId::new(404)
        ));

        let id = database
            .import_books(&[record("/books/dune.epub", "Dune", "Frank Herbert")])
            .expect("import book")[0];
        let missing = temporary_asset_path("missing-attachment");
        assert!(matches!(
            database.attach_reference_asset(id, BookFormat::Pdf, &missing),
            Err(StorageError::AttachmentUnavailable(AssetHealth::Missing))
        ));
    }

    #[test]
    fn removing_a_book_cascades_library_data_without_deleting_source_files() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let source = TestAsset::file("remove-source");
        let mut imported = record(
            source.path().to_str().expect("UTF-8 test path"),
            "Dune",
            "Frank Herbert",
        );
        imported.cover_thumbnail = Some(vec![1, 2, 3]);
        let id = database.import_books(&[imported]).expect("import book")[0];

        assert!(database.remove_book(id).expect("remove book"));

        let assets: i64 = database
            .connection
            .query_row("SELECT count(*) FROM book_assets", [], |row| row.get(0))
            .expect("count assets");
        assert_eq!(assets, 0);
        assert_eq!(database.load_cover(id).expect("load cover"), None);
        assert_eq!(database.count().expect("count books"), 0);
        assert_eq!(database.get_book(id).expect("load removed book"), None);
        assert!(
            database
                .query(&LibraryQuery {
                    search: "Dune".into(),
                    ..LibraryQuery::default()
                })
                .expect("search removed book")
                .is_empty()
        );
        assert_eq!(
            fs::read(source.path()).expect("read source"),
            b"publication"
        );
        assert!(!database.remove_book(id).expect("remove absent book"));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn bulk_removal_is_atomic_exact_generation_safe_and_keeps_source_files() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let source = TestAsset::file("bulk-remove-source");
        let mut target_ids = Vec::new();
        for index in 0..12 {
            let title = if index < 10 {
                format!("Target {index}")
            } else {
                format!("Other {index}")
            };
            let path = if index == 0 {
                source.path().to_str().expect("UTF-8 test path").to_owned()
            } else {
                format!("/books/bulk-remove-{index}.epub")
            };
            let mut imported = record(&path, &title, "Bulk Author");
            if index == 0 {
                imported.cover_thumbnail = Some(vec![1, 2, 3]);
            }
            let id = database.import_books(&[imported]).expect("seed book")[0];
            if index < 10 {
                target_ids.push(id);
            }
        }
        let target_query = LibraryQuery {
            search: "title:Target".into(),
            ..LibraryQuery::default()
        };
        let snapshot = database
            .selection_snapshot(&target_query)
            .expect("selection snapshot");
        assert_eq!(snapshot.matching_books, 10);
        let excluded = target_ids[3];
        let selection =
            BookSelection::all_matching(target_query.clone(), snapshot.generation, vec![excluded]);

        let result = database
            .remove_books(&selection)
            .expect("bulk remove books");
        assert_eq!(result.books_removed, 9);
        assert_eq!(database.count().expect("count books"), 3);
        assert!(
            database
                .get_book(excluded)
                .expect("load exclusion")
                .is_some()
        );
        assert_eq!(
            database.load_cover(target_ids[0]).expect("load cover"),
            None
        );
        assert_eq!(
            database
                .query(&target_query)
                .expect("search remaining targets")
                .iter()
                .map(|book| book.id)
                .collect::<Vec<_>>(),
            vec![excluded]
        );
        assert_eq!(
            fs::read(source.path()).expect("read source"),
            b"publication"
        );

        let assets: i64 = database
            .connection
            .query_row("SELECT count(*) FROM book_assets", [], |row| row.get(0))
            .expect("count assets");
        assert_eq!(assets, 3);
        let removal_triggers: i64 = database
            .connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema \
                 WHERE type = 'trigger' AND name IN ( \
                     'book_covers_after_delete_summary', \
                     'book_assets_after_delete_summary', \
                     'books_after_delete' \
                 )",
                [],
                |row| row.get(0),
            )
            .expect("count restored removal triggers");
        assert_eq!(removal_triggers, 3);
        database
            .connection
            .execute(
                "INSERT INTO books_fts(books_fts, rank) VALUES ('integrity-check', 1)",
                [],
            )
            .expect("validate FTS after bulk removal");

        let stale_snapshot = database
            .selection_snapshot(&target_query)
            .expect("fresh snapshot");
        let stale =
            BookSelection::all_matching(target_query, stale_snapshot.generation, Vec::new());
        database
            .import_books(&[record(
                "/books/bulk-remove-generation.epub",
                "Generation change",
                "Bulk Author",
            )])
            .expect("change generation");
        assert!(matches!(
            database.remove_books(&stale),
            Err(StorageError::StaleSelection)
        ));
        assert!(
            database
                .get_book(excluded)
                .expect("load exclusion")
                .is_some()
        );

        let explicit = BookSelection::explicit(vec![excluded, excluded, BookId::new(404)]);
        assert_eq!(
            database
                .remove_books(&explicit)
                .expect("remove explicit selection")
                .books_removed,
            1
        );
        assert_eq!(
            database
                .remove_books(&BookSelection::explicit(Vec::new()))
                .expect("remove empty selection")
                .books_removed,
            0
        );
    }

    #[test]
    fn unknown_stored_formats_are_not_silently_mislabeled() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let id = database
            .import_books(&[record("/books/dune.epub", "Dune", "Frank Herbert")])
            .expect("import book")[0];
        database
            .connection
            .execute(
                "UPDATE book_assets SET format = 'azw3' WHERE book_id = ?1",
                [id.value()],
            )
            .expect("store future format");

        assert!(matches!(
            database.get_book(id),
            Err(StorageError::InvalidAssetFormat(format)) if format == "azw3"
        ));
    }

    #[test]
    fn format_filter_drives_from_the_covering_asset_index() {
        let database = LibraryDatabase::open_in_memory().expect("open library");
        let mut statement = database
            .connection
            .prepare(
                "EXPLAIN QUERY PLAN \
                 SELECT b.id FROM books b \
                 JOIN book_assets filtered_assets \
                   ON filtered_assets.book_id = b.id \
                  AND filtered_assets.format = 'epub' \
                 ORDER BY b.sort_title, b.id",
            )
            .expect("prepare query plan");
        let details = statement
            .query_map([], |row| row.get::<_, String>(3))
            .expect("explain query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect query plan");

        assert!(
            details.iter().any(|detail| {
                detail.contains("SEARCH filtered_assets USING COVERING INDEX")
                    && detail.contains("book_assets_format_book_idx")
                    && detail.contains("format=?)")
            }),
            "unexpected query plan: {details:?}"
        );
    }

    #[test]
    fn asset_health_filter_drives_from_the_covering_asset_index() {
        let database = LibraryDatabase::open_in_memory().expect("open library");
        let mut statement = database
            .connection
            .prepare(
                "EXPLAIN QUERY PLAN \
                 SELECT book_id FROM book_assets WHERE health = 'missing' GROUP BY book_id",
            )
            .expect("prepare query plan");
        let details = statement
            .query_map([], |row| row.get::<_, String>(3))
            .expect("explain query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect query plan");

        assert!(
            details.iter().any(|detail| {
                detail.contains("SEARCH book_assets USING COVERING INDEX")
                    && detail.contains("book_assets_health_book_idx")
                    && detail.contains("health=?)")
            }),
            "unexpected query plan: {details:?}"
        );
    }

    #[test]
    fn book_summary_state_tracks_cover_and_asset_health_changes() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let imported = BookImport {
            book: metadata("Dune", "Frank Herbert"),
            assets: vec![
                asset("/books/dune.epub", BookFormat::Epub),
                asset("/books/dune.pdf", BookFormat::Pdf),
            ],
            cover_thumbnail: Some(vec![1, 2, 3]),
        };
        let book_id = database.import_books(&[imported]).expect("import book")[0];
        let assets = database
            .get_book(book_id)
            .expect("load book")
            .expect("book exists")
            .assets;
        let summary = || {
            database
                .query(&LibraryQuery::default())
                .expect("query summary")
                .into_iter()
                .next()
                .expect("summary exists")
        };

        assert!(summary().has_cover);
        assert!(!summary().has_file_issue);

        database
            .connection
            .execute(
                "UPDATE book_assets SET health = 'missing' WHERE id = ?1",
                [assets[0].id.value()],
            )
            .expect("mark first asset missing");
        database
            .connection
            .execute(
                "UPDATE book_assets SET health = 'unreadable' WHERE id = ?1",
                [assets[1].id.value()],
            )
            .expect("mark second asset unreadable");
        assert!(summary().has_file_issue);

        database
            .connection
            .execute(
                "UPDATE book_assets SET health = 'available' WHERE id = ?1",
                [assets[0].id.value()],
            )
            .expect("restore first asset");
        assert!(summary().has_file_issue);

        database
            .connection
            .execute(
                "DELETE FROM book_assets WHERE id = ?1",
                [assets[1].id.value()],
            )
            .expect("delete remaining issue");
        assert!(!summary().has_file_issue);

        database
            .connection
            .execute(
                "DELETE FROM book_covers WHERE book_id = ?1",
                [book_id.value()],
            )
            .expect("delete cover");
        assert!(!summary().has_cover);
    }

    #[test]
    fn book_summary_projection_does_not_probe_asset_tables() {
        let database = LibraryDatabase::open_in_memory().expect("open library");
        let mut statement = database
            .connection
            .prepare(
                "EXPLAIN QUERY PLAN \
                 SELECT b.id, b.title, b.authors, b.series, \
                        b.has_cover, b.has_file_issue \
                 FROM books b ORDER BY b.sort_title, b.id",
            )
            .expect("prepare query plan");
        let details = statement
            .query_map([], |row| row.get::<_, String>(3))
            .expect("explain query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect query plan");

        assert!(
            details.iter().all(|detail| {
                !detail.contains("book_covers")
                    && !detail.contains("book_assets")
                    && !detail.contains("CORRELATED")
            }),
            "unexpected query plan: {details:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_reference_paths_round_trip_without_loss() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let path = PathBuf::from(OsString::from_vec(b"/books/dune-\xff.epub".to_vec()));
        let imported = BookImport {
            book: metadata("Dune", "Frank Herbert"),
            assets: vec![asset(path.clone(), BookFormat::Epub)],
            cover_thumbnail: None,
        };

        let id = database
            .import_books(&[imported])
            .expect("import non-UTF-8 path")[0];
        let stored = database
            .get_book(id)
            .expect("load book")
            .expect("book exists");

        assert_eq!(stored.assets[0].path, path);
    }

    #[test]
    fn invalid_windows_path_bytes_are_rejected() {
        assert!(matches!(
            decode_path("windows", vec![0]),
            Err(StorageError::InvalidPathData(encoding)) if encoding == "windows"
        ));
    }

    #[test]
    fn failed_migration_rolls_back_schema_data_and_search_index() {
        let database_file = TestDatabase::new("migration-rollback");
        create_legacy_library(database_file.path(), 2, BookFormat::Epub);
        let connection = Connection::open(database_file.path()).expect("open legacy database");
        connection
            .execute("UPDATE books SET source_path = '' WHERE id = 7", [])
            .expect("corrupt legacy path");
        drop(connection);

        assert!(LibraryDatabase::open(database_file.path()).is_err());

        let connection = Connection::open(database_file.path()).expect("reopen rolled-back file");
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("read rolled-back version");
        assert_eq!(version, 2);
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM pragma_table_info('books') \
                     WHERE name = 'source_path'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("check legacy column"),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM sqlite_schema \
                     WHERE type = 'table' AND name = 'book_assets'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("check asset table absence"),
            0
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM books_fts WHERE books_fts MATCH 'Dune'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("search rolled-back FTS index"),
            1
        );
    }

    fn earthsea_edit(id: BookId) -> BookEdit {
        BookEdit {
            id,
            title: "A Wizard of Earthsea".into(),
            publisher: Some("Parnassus".into()),
            language: Some("EN".into()),
            description: Some("An archipelago tale".into()),
            contributors: vec![
                ContributorCreditEdit {
                    contributor: ContributorReference::New {
                        display_name: "Ursula K. Le Guin".into(),
                        sort_name: "Le Guin, Ursula K.".into(),
                    },
                    role: ContributorRole::Author,
                    position: 0,
                },
                ContributorCreditEdit {
                    contributor: ContributorReference::New {
                        display_name: "Ruth Robbins".into(),
                        sort_name: "Robbins, Ruth".into(),
                    },
                    role: ContributorRole::Illustrator,
                    position: 0,
                },
            ],
            series: Some(SeriesMembershipEdit {
                series: SeriesReference::New("Earthsea Cycle".into()),
                index: Some("1.250000".parse::<SeriesIndex>().expect("valid index")),
            }),
            tags: vec![
                TagReference::New("Science Fiction".into()),
                TagReference::New(" Fantasy ".into()),
            ],
            identifiers: Vec::new(),
        }
    }

    #[test]
    fn normalized_book_edit_updates_detail_projection_search_and_facets_atomically() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let id = database
            .import_books(&[record(
                "/books/earthsea.epub",
                "Legacy title",
                "Combined author",
            )])
            .expect("import book")[0];
        let original_asset = database
            .get_book(id)
            .expect("load original")
            .expect("book exists")
            .assets;
        let edit = earthsea_edit(id);

        database
            .save_book_edit(&edit)
            .expect("save normalized edit");
        let stored = database
            .get_book(id)
            .expect("reload edited book")
            .expect("book exists");

        assert_eq!(stored.authors, "Ursula K. Le Guin");
        assert_eq!(stored.contributors.len(), 2);
        assert_eq!(stored.contributors[1].role, ContributorRole::Illustrator);
        assert_eq!(
            stored
                .series_membership
                .as_ref()
                .and_then(|membership| membership.index)
                .map(|index| index.to_string())
                .as_deref(),
            Some("1.25")
        );
        assert_eq!(
            stored
                .tags
                .iter()
                .map(|tag| tag.name.as_str())
                .collect::<Vec<_>>(),
            ["Fantasy", "Science Fiction"]
        );
        assert_eq!(stored.assets, original_asset);

        let contributor = stored.contributors[0].contributor.id;
        let series = stored.series_membership.as_ref().expect("series").series.id;
        let science_fiction = stored
            .tags
            .iter()
            .find(|tag| tag.name == "Science Fiction")
            .expect("science fiction tag")
            .id;
        let results = database
            .query(&LibraryQuery {
                search: "author:ursula tag:\"science fiction\" language:en format:epub".into(),
                facets: ExactFacets::new(
                    vec![ContributorFacet {
                        contributor,
                        author_only: true,
                    }],
                    Some(series),
                    vec![science_fiction],
                    Vec::new(),
                )
                .expect("valid facets"),
                sort: SortOrder::Series,
                ..LibraryQuery::default()
            })
            .expect("run structured exact query");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, id);
        assert_eq!(
            results[0]
                .series_index
                .map(|index| index.to_string())
                .as_deref(),
            Some("1.25")
        );
    }

    #[test]
    fn identifiers_store_default_and_custom_types_with_multiple_values() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let defaults = database
            .autocomplete_identifier_types("", 50)
            .expect("load default identifier types");
        assert_eq!(defaults.len(), 10);
        assert!(
            defaults
                .iter()
                .any(|usage| usage.identifier_type.name == "ISBN")
        );

        let id = database
            .import_books(&[record("/books/identifiers.epub", "Identifiers", "Author")])
            .expect("import book")[0];
        let mut edit = earthsea_edit(id);
        edit.identifiers = vec![
            BookIdentifierEdit {
                identifier_type: IdentifierTypeReference::New(" ISBN ".into()),
                value: " 978-0-1234-5678-9 ".into(),
            },
            BookIdentifierEdit {
                identifier_type: IdentifierTypeReference::New("Library Source".into()),
                value: "Record-42".into(),
            },
        ];
        database.save_book_edit(&edit).expect("save identifiers");

        let stored = database.get_book(id).unwrap().unwrap();
        assert_eq!(stored.identifiers.len(), 2);
        let isbn = stored
            .identifiers
            .iter()
            .find(|identifier| identifier.identifier_type.name == "ISBN")
            .expect("stored ISBN");
        assert_eq!(isbn.value, "978-0-1234-5678-9");
        let isbn_type = isbn.identifier_type.id;

        edit.identifiers.push(BookIdentifierEdit {
            identifier_type: IdentifierTypeReference::Existing(isbn_type),
            value: "978-0-9876-5432-1".into(),
        });
        database.save_book_edit(&edit).expect("save a second ISBN");
        let stored = database.get_book(id).unwrap().unwrap();
        assert_eq!(
            stored
                .identifiers
                .iter()
                .filter(|identifier| identifier.identifier_type.id == isbn_type)
                .count(),
            2
        );
        let suggestions = database
            .autocomplete_identifier_types("isbn", 50)
            .expect("autocomplete ISBN");
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].identifier_type.id, isbn_type);
        assert_eq!(suggestions[0].identifiers, 2);
    }

    #[test]
    fn invalid_identifier_rolls_back_its_new_type() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let id = database
            .import_books(&[record("/books/invalid-identifier.epub", "Book", "Author")])
            .expect("import book")[0];
        let mut edit = earthsea_edit(id);
        edit.identifiers = vec![BookIdentifierEdit {
            identifier_type: IdentifierTypeReference::New("Should Roll Back".into()),
            value: " \n ".into(),
        }];

        assert!(matches!(
            database.save_book_edit(&edit),
            Err(StorageError::InvalidCuration(message)) if message.contains("identifier value")
        ));
        assert!(
            database
                .autocomplete_identifier_types("Should", 50)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn series_book_numbers_are_unique_with_self_excluding_availability_checks() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let ids = database
            .import_books(&[
                record("/books/earthsea-one.epub", "First", "Author"),
                record("/books/earthsea-two.epub", "Second", "Author"),
            ])
            .expect("import books");
        database
            .save_book_edit(&earthsea_edit(ids[0]))
            .expect("save first series member");
        let series = database
            .get_book(ids[0])
            .expect("load first book")
            .expect("first book exists")
            .series_membership
            .expect("first book has series")
            .series
            .id;
        let index = "1.25".parse::<SeriesIndex>().expect("valid series index");

        assert!(
            !database
                .series_index_is_available(series, index, ids[1])
                .expect("check conflicting number")
        );
        let mut second = earthsea_edit(ids[1]);
        second.title = "The Tombs of Atuan".into();
        second.series = Some(SeriesMembershipEdit {
            series: SeriesReference::Existing(series),
            index: Some(index),
        });
        assert!(matches!(
            database.save_book_edit(&second),
            Err(StorageError::InvalidCuration(message))
                if message.contains("book number 1.25 is already used")
        ));

        let second_index = "2".parse::<SeriesIndex>().expect("valid second index");
        second.series = Some(SeriesMembershipEdit {
            series: SeriesReference::Existing(series),
            index: Some(second_index),
        });
        database
            .save_book_edit(&second)
            .expect("save distinct series number");
        assert!(
            database
                .series_index_is_available(series, second_index, ids[1])
                .expect("exclude current book")
        );
    }

    #[test]
    fn invalid_normalized_edit_rolls_back_entities_metadata_and_projection() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let id = database
            .import_books(&[record(
                "/books/rollback.epub",
                "Original",
                "Original Author",
            )])
            .expect("import book")[0];
        let before = database
            .get_book(id)
            .expect("load original")
            .expect("book exists");
        let invalid = BookEdit {
            id,
            title: "Must roll back".into(),
            publisher: None,
            language: None,
            description: None,
            contributors: vec![
                ContributorCreditEdit {
                    contributor: ContributorReference::New {
                        display_name: "Duplicate Person".into(),
                        sort_name: "Duplicate Person".into(),
                    },
                    role: ContributorRole::Author,
                    position: 0,
                },
                ContributorCreditEdit {
                    contributor: ContributorReference::New {
                        display_name: " duplicate   person ".into(),
                        sort_name: "Duplicate Person".into(),
                    },
                    role: ContributorRole::Author,
                    position: 1,
                },
            ],
            series: None,
            tags: vec![TagReference::New("Should Not Persist".into())],
            identifiers: Vec::new(),
        };

        assert!(matches!(
            database.save_book_edit(&invalid),
            Err(StorageError::InvalidCuration(_))
        ));
        assert_eq!(
            database
                .get_book(id)
                .expect("reload after rollback")
                .expect("book exists"),
            before
        );
        assert!(
            database
                .autocomplete_tags("Should", &[], 50)
                .unwrap()
                .is_empty()
        );
        assert!(
            database
                .query(&LibraryQuery {
                    search: "Must".into(),
                    ..LibraryQuery::default()
                })
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn bounded_autocomplete_reuses_identity_and_puts_selected_values_first() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let id = database
            .import_books(&[record("/books/autocomplete.epub", "Book", "Legacy")])
            .expect("import book")[0];
        database
            .save_book_edit(&BookEdit {
                id,
                title: "Book".into(),
                publisher: None,
                language: None,
                description: None,
                contributors: vec![ContributorCreditEdit {
                    contributor: ContributorReference::New {
                        display_name: "Alpha Writer".into(),
                        sort_name: "Writer, Alpha".into(),
                    },
                    role: ContributorRole::Author,
                    position: 0,
                }],
                series: Some(SeriesMembershipEdit {
                    series: SeriesReference::New("Alpha Series".into()),
                    index: None,
                }),
                tags: vec![
                    TagReference::New("Alpha Tag".into()),
                    TagReference::New("Alpine".into()),
                ],
                identifiers: Vec::new(),
            })
            .expect("save curation");
        let book = database.get_book(id).unwrap().unwrap();
        let selected_tag = book
            .tags
            .iter()
            .find(|tag| tag.name == "Alpine")
            .expect("selected tag")
            .id;

        let tags = database
            .autocomplete_tags("alpha", &[selected_tag], 2)
            .expect("autocomplete tags");
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[0].tag.id, selected_tag);
        assert_eq!(tags[1].tag.name, "Alpha Tag");
        assert_eq!(tags[1].books, 1);
        assert_eq!(
            database
                .autocomplete_contributors("alpha", &[], 50)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            database
                .autocomplete_series("alpha", &[], 50)
                .unwrap()
                .len(),
            1
        );
        assert!(database.autocomplete_tags("", &[], 500).unwrap().len() <= 50);
    }

    #[test]
    fn vocabulary_manager_searches_indexed_pages_without_rendering_over_one_hundred_rows() {
        let database = LibraryDatabase::open_in_memory().expect("open library");
        for index in 0..125 {
            let display = format!("Writer {index:03}");
            let key = format!("writer {index:03}");
            database
                .connection
                .execute(
                    "INSERT INTO contributors( \
                         display_name, sort_name, identity_key, sort_key \
                     ) VALUES (?1, ?1, ?2, ?2)",
                    params![display, key],
                )
                .unwrap();
            let display = format!("Series {index:03}");
            let key = format!("series {index:03}");
            database
                .connection
                .execute(
                    "INSERT INTO series_entities(name, identity_key) VALUES (?1, ?2)",
                    params![display, key],
                )
                .unwrap();
            let display = format!("Tag {index:03}");
            let key = format!("tag {index:03}");
            database
                .connection
                .execute(
                    "INSERT INTO tags(name, identity_key) VALUES (?1, ?2)",
                    params![display, key],
                )
                .unwrap();
        }

        let first = database.search_tags("", 0, 500).unwrap();
        let second = database.search_tags("", 100, 500).unwrap();
        assert_eq!(first.len(), 100);
        assert_eq!(second.len(), 25);
        assert_eq!(first[0].tag.name, "Tag 000");
        assert_eq!(second[0].tag.name, "Tag 100");
        assert_eq!(second[24].tag.name, "Tag 124");
        assert!(first.iter().all(|usage| usage.books == 0));
        assert!(first.iter().all(|usage| usage.saved_searches == 0));

        let contributors = database.search_contributors("writer 01", 0, 100).unwrap();
        assert_eq!(contributors.len(), 10);
        assert_eq!(contributors[0].contributor.display_name, "Writer 010");
        assert!(contributors.iter().all(|usage| usage.books == 0));
        let series = database.search_series("series 12", 0, 100).unwrap();
        assert_eq!(series.len(), 5);
        assert_eq!(series[4].series.name, "Series 124");
        assert!(database.search_tags("", u64::MAX, 100).unwrap().is_empty());
        assert!(matches!(
            database.search_contributors("bad\nprefix", 0, 100),
            Err(StorageError::InvalidCuration(_))
        ));

        for table in ["contributors", "series_entities", "tags"] {
            let details = database
                .connection
                .prepare(&format!(
                    "EXPLAIN QUERY PLAN SELECT id FROM {table} \
                     WHERE identity_key >= ?1 AND identity_key < ?2 \
                     ORDER BY identity_key, id LIMIT 100 OFFSET 0"
                ))
                .unwrap()
                .query_map(["a", "b"], |row| row.get::<_, String>(3))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
                .join("\n");
            assert!(details.contains("COVERING INDEX"), "{details}");
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn query_backed_bulk_tags_are_atomic_exact_and_generation_safe() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        for index in 0..12 {
            let title = if index < 10 {
                format!("Target {index}")
            } else {
                format!("Other {index}")
            };
            database
                .import_books(&[record(
                    &format!("/books/bulk-{index}.epub"),
                    &title,
                    "Bulk Author",
                )])
                .expect("seed book");
        }
        let target_query = LibraryQuery {
            search: "title:Target".into(),
            ..LibraryQuery::default()
        };
        let snapshot = database
            .selection_snapshot(&target_query)
            .expect("selection snapshot");
        assert_eq!(snapshot.matching_books, 10);
        let target_ids = database
            .query_ids_window(&target_query, 0, 20)
            .expect("compact IDs");
        assert_eq!(target_ids.len(), 10);
        let excluded = target_ids[3];
        let selection =
            BookSelection::all_matching(target_query.clone(), snapshot.generation, vec![excluded]);

        let result = database
            .apply_bulk_tags(
                &selection,
                &BulkTagEdit {
                    add: vec![
                        TagReference::New("Science Fiction".into()),
                        TagReference::New("Favourite".into()),
                    ],
                    remove: Vec::new(),
                },
            )
            .expect("bulk add tags");
        assert_eq!(result.books_matched, 9);
        assert_eq!(result.relationships_added, 18);
        assert_eq!(result.relationships_removed, 0);
        assert_eq!(result.tags_created, 2);

        let science = database
            .autocomplete_tags("Science Fiction", &[], 50)
            .unwrap()
            .into_iter()
            .find(|usage| usage.tag.name == "Science Fiction")
            .expect("created tag");
        let favourite = database
            .autocomplete_tags("Favourite", &[], 50)
            .unwrap()
            .into_iter()
            .find(|usage| usage.tag.name == "Favourite")
            .expect("created tag");
        assert_eq!(science.books, 9);
        assert_eq!(favourite.books, 9);
        assert_eq!(
            database
                .query(&LibraryQuery {
                    facets: ExactFacets::new(
                        Vec::new(),
                        None,
                        vec![science.tag.id, favourite.tag.id],
                        Vec::new(),
                    )
                    .unwrap(),
                    ..LibraryQuery::default()
                })
                .unwrap()
                .len(),
            9
        );
        assert_eq!(
            database
                .query(&LibraryQuery {
                    search: "tag:\"Science Fiction\"".into(),
                    ..LibraryQuery::default()
                })
                .unwrap()
                .len(),
            9
        );

        let explicit = BookSelection::explicit(target_ids.clone());
        let before_failure = database
            .selection_tag_usage(&explicit, 0, 100)
            .expect("tag states before failure");
        assert!(
            database
                .apply_bulk_tags(
                    &explicit,
                    &BulkTagEdit {
                        add: vec![TagReference::New("Rollback tag".into())],
                        remove: vec![science.tag.id],
                    }
                )
                .is_ok()
        );
        let after_success = database
            .selection_tag_usage(&explicit, 0, 100)
            .expect("tag states after success");
        assert_ne!(before_failure, after_success);

        let fresh = database.selection_snapshot(&target_query).unwrap();
        let stale = BookSelection::all_matching(target_query.clone(), fresh.generation, Vec::new());
        let mut changed = database.get_book(target_ids[0]).unwrap().unwrap();
        changed.title = "Changed target".into();
        database.save_book(&changed).expect("mutate generation");
        assert!(matches!(
            database.apply_bulk_tags(&stale, &BulkTagEdit::default()),
            Err(StorageError::StaleSelection)
        ));

        let current = database.selection_snapshot(&target_query).unwrap();
        let current = BookSelection::all_matching(target_query, current.generation, Vec::new());
        let rollback_before = database.selection_tag_usage(&current, 0, 100).unwrap();
        assert!(matches!(
            database.apply_bulk_tags(
                &current,
                &BulkTagEdit {
                    add: vec![TagReference::New("Invalid\nTag".into())],
                    remove: vec![favourite.tag.id],
                }
            ),
            Err(StorageError::InvalidCuration(_))
        ));
        assert_eq!(
            database.selection_tag_usage(&current, 0, 100).unwrap(),
            rollback_before
        );
        assert!(
            database
                .autocomplete_tags("Invalid", &[], 50)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn saved_searches_round_trip_complete_canonical_projections() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let id = database
            .import_books(&[record(
                "/books/saved-search.epub",
                "Legacy title",
                "Combined author",
            )])
            .expect("import book")[0];
        database
            .save_book_edit(&earthsea_edit(id))
            .expect("save normalized entities");
        let book = database.get_book(id).unwrap().unwrap();
        let contributor = book.contributors[0].contributor.id;
        let series = book.series_membership.as_ref().unwrap().series.id;
        let included = book
            .tags
            .iter()
            .find(|tag| tag.name == "Science Fiction")
            .unwrap()
            .id;
        let excluded = book
            .tags
            .iter()
            .find(|tag| tag.name == "Fantasy")
            .unwrap()
            .id;
        let query = LibraryQuery {
            search: " title:\"A Wizard\"   language:en ".into(),
            format: Some(BookFormat::Epub),
            asset_health: Some(AssetHealth::Available),
            facets: ExactFacets::new(
                vec![ContributorFacet {
                    contributor,
                    author_only: true,
                }],
                Some(series),
                vec![included],
                vec![excluded],
            )
            .unwrap(),
            sort: SortOrder::Series,
        };

        let saved_id = database
            .create_saved_search("  Earthsea   reference  ", &query)
            .expect("create saved search");
        let saved = database.list_saved_searches().expect("list saved searches");
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].id, saved_id);
        assert_eq!(saved[0].name, "Earthsea reference");
        assert_eq!(
            saved[0].query.search,
            SearchExpression::parse(&query.search).unwrap().canonical()
        );
        assert_eq!(saved[0].query.format, query.format);
        assert_eq!(saved[0].query.asset_health, query.asset_health);
        assert_eq!(saved[0].query.facets, query.facets);
        assert_eq!(saved[0].query.sort, query.sort);

        let replacement = LibraryQuery {
            search: "publisher:Parnassus".into(),
            format: None,
            asset_health: Some(AssetHealth::Unknown),
            facets: ExactFacets::default(),
            sort: SortOrder::RecentlyAdded,
        };
        database
            .update_saved_search(saved_id, &replacement)
            .expect("explicit update");
        database
            .rename_saved_search(saved_id, "Zeta")
            .expect("rename with stable identity");
        let first_id = database
            .create_saved_search("Alpha", &LibraryQuery::default())
            .expect("create alphabetically first search");
        let saved = database.list_saved_searches().unwrap();
        assert_eq!(
            saved.iter().map(|value| value.id).collect::<Vec<_>>(),
            [first_id, saved_id]
        );
        assert_eq!(saved[1].query, replacement);
        assert_eq!(
            database
                .search_saved_searches("", 0, 1)
                .unwrap()
                .iter()
                .map(|value| value.id)
                .collect::<Vec<_>>(),
            [first_id]
        );
        assert_eq!(
            database.search_saved_searches("zet", 0, 100).unwrap(),
            [saved[1].clone()]
        );
        assert!(matches!(
            database.search_saved_searches("bad\nprefix", 0, 100),
            Err(StorageError::InvalidCuration(_))
        ));

        let before_collision = saved.clone();
        assert!(matches!(
            database.rename_saved_search(saved_id, " alpha "),
            Err(StorageError::InvalidCuration(_))
        ));
        assert_eq!(database.list_saved_searches().unwrap(), before_collision);

        let before_invalid_update = database.list_saved_searches().unwrap();
        assert!(matches!(
            database.update_saved_search(
                saved_id,
                &LibraryQuery {
                    search: "title:".into(),
                    ..LibraryQuery::default()
                }
            ),
            Err(StorageError::InvalidSearch(_))
        ));
        assert_eq!(
            database.list_saved_searches().unwrap(),
            before_invalid_update
        );

        let before_missing_facet = database.list_saved_searches().unwrap();
        assert!(
            database
                .update_saved_search(
                    saved_id,
                    &LibraryQuery {
                        facets: ExactFacets::new(
                            Vec::new(),
                            None,
                            vec![TagId::new(i64::MAX)],
                            Vec::new(),
                        )
                        .unwrap(),
                        ..LibraryQuery::default()
                    }
                )
                .is_err()
        );
        assert_eq!(
            database.list_saved_searches().unwrap(),
            before_missing_facet
        );

        let book_before_delete = database.get_book(id).unwrap().unwrap();
        let tags_before_delete = database.autocomplete_tags("", &[], 50).unwrap();
        assert!(
            !database
                .delete_saved_search(SavedSearchId::new(404))
                .unwrap()
        );
        assert!(database.delete_saved_search(saved_id).unwrap());
        assert_eq!(database.get_book(id).unwrap().unwrap(), book_before_delete);
        assert_eq!(
            database.autocomplete_tags("", &[], 50).unwrap(),
            tags_before_delete
        );
        assert_eq!(
            database
                .list_saved_searches()
                .unwrap()
                .iter()
                .map(|value| value.id)
                .collect::<Vec<_>>(),
            [first_id]
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn vocabulary_mutations_rewrite_relations_saved_facets_and_projections_atomically() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let first = database
            .import_books(&[record("/books/merge-first.epub", "First", "Legacy")])
            .unwrap()[0];
        let second = database
            .import_books(&[record("/books/merge-second.epub", "Second", "Legacy")])
            .unwrap()[0];
        database
            .save_book_edit(&BookEdit {
                id: first,
                title: "First".into(),
                publisher: Some("Publisher".into()),
                language: Some("en".into()),
                description: None,
                contributors: vec![
                    ContributorCreditEdit {
                        contributor: ContributorReference::New {
                            display_name: "Source Writer".into(),
                            sort_name: "Writer, Source".into(),
                        },
                        role: ContributorRole::Author,
                        position: 0,
                    },
                    ContributorCreditEdit {
                        contributor: ContributorReference::New {
                            display_name: "Other Writer".into(),
                            sort_name: "Writer, Other".into(),
                        },
                        role: ContributorRole::Author,
                        position: 1,
                    },
                    ContributorCreditEdit {
                        contributor: ContributorReference::New {
                            display_name: "Target Writer".into(),
                            sort_name: "Writer, Target".into(),
                        },
                        role: ContributorRole::Author,
                        position: 2,
                    },
                    ContributorCreditEdit {
                        contributor: ContributorReference::New {
                            display_name: "Source Writer".into(),
                            sort_name: "Writer, Source".into(),
                        },
                        role: ContributorRole::Editor,
                        position: 0,
                    },
                ],
                series: Some(SeriesMembershipEdit {
                    series: SeriesReference::New("Source Series".into()),
                    index: Some("1.5".parse().unwrap()),
                }),
                tags: vec![
                    TagReference::New("Source Tag".into()),
                    TagReference::New("Target Tag".into()),
                ],
                identifiers: Vec::new(),
            })
            .unwrap();
        let seeded_first = database.get_book(first).unwrap().unwrap();
        let source_contributor = seeded_first
            .contributors
            .iter()
            .find(|credit| credit.contributor.display_name == "Source Writer")
            .unwrap()
            .contributor
            .id;
        let target_contributor = seeded_first
            .contributors
            .iter()
            .find(|credit| credit.contributor.display_name == "Target Writer")
            .unwrap()
            .contributor
            .id;
        let source_series = seeded_first.series_membership.as_ref().unwrap().series.id;
        let source_tag = seeded_first
            .tags
            .iter()
            .find(|tag| tag.name == "Source Tag")
            .unwrap()
            .id;
        let target_tag = seeded_first
            .tags
            .iter()
            .find(|tag| tag.name == "Target Tag")
            .unwrap()
            .id;
        database
            .save_book_edit(&BookEdit {
                id: second,
                title: "Second".into(),
                publisher: None,
                language: Some("en".into()),
                description: None,
                contributors: vec![
                    ContributorCreditEdit {
                        contributor: ContributorReference::Existing(target_contributor),
                        role: ContributorRole::Author,
                        position: 0,
                    },
                    ContributorCreditEdit {
                        contributor: ContributorReference::Existing(source_contributor),
                        role: ContributorRole::Author,
                        position: 1,
                    },
                ],
                series: Some(SeriesMembershipEdit {
                    series: SeriesReference::Existing(source_series),
                    index: Some("2".parse().unwrap()),
                }),
                tags: vec![TagReference::Existing(source_tag)],
                identifiers: Vec::new(),
            })
            .unwrap();
        let target_series = {
            database
                .connection
                .execute(
                    "INSERT INTO series_entities(name, identity_key) VALUES ('Target Series', \
                     'target series')",
                    [],
                )
                .unwrap();
            lectern_core::organisation::SeriesId::new(database.connection.last_insert_rowid())
        };

        let saved_source = database
            .create_saved_search(
                "Source facets",
                &LibraryQuery {
                    facets: ExactFacets::new(
                        vec![ContributorFacet {
                            contributor: source_contributor,
                            author_only: false,
                        }],
                        Some(source_series),
                        vec![source_tag],
                        Vec::new(),
                    )
                    .unwrap(),
                    ..LibraryQuery::default()
                },
            )
            .unwrap();
        let saved_target_wins = database
            .create_saved_search(
                "Target include wins",
                &LibraryQuery {
                    facets: ExactFacets::new(
                        vec![
                            ContributorFacet {
                                contributor: target_contributor,
                                author_only: true,
                            },
                            ContributorFacet {
                                contributor: source_contributor,
                                author_only: false,
                            },
                        ],
                        None,
                        vec![target_tag],
                        vec![source_tag],
                    )
                    .unwrap(),
                    ..LibraryQuery::default()
                },
            )
            .unwrap();
        let saved_exclusion_wins = database
            .create_saved_search(
                "Target exclusion wins",
                &LibraryQuery {
                    facets: ExactFacets::new(Vec::new(), None, vec![source_tag], vec![target_tag])
                        .unwrap(),
                    ..LibraryQuery::default()
                },
            )
            .unwrap();
        assert_eq!(
            database
                .contributor_mutation_impact(source_contributor)
                .unwrap(),
            VocabularyMutationResult {
                books: 2,
                saved_searches: 2
            }
        );
        assert_eq!(
            database.series_mutation_impact(source_series).unwrap(),
            VocabularyMutationResult {
                books: 2,
                saved_searches: 1
            }
        );
        assert_eq!(
            database.tag_mutation_impact(source_tag).unwrap(),
            VocabularyMutationResult {
                books: 2,
                saved_searches: 3
            }
        );
        let assets_before = [
            database.get_book(first).unwrap().unwrap().assets,
            database.get_book(second).unwrap().unwrap().assets,
        ];

        database
            .rename_contributor(source_contributor, "Renamed Source", "Source, Renamed")
            .unwrap();
        assert_eq!(
            database
                .query(&LibraryQuery {
                    search: "author:\"Renamed Source\"".into(),
                    ..LibraryQuery::default()
                })
                .unwrap()
                .len(),
            2
        );
        let before_collision = database.get_book(first).unwrap().unwrap();
        assert!(matches!(
            database.rename_contributor(source_contributor, " target   writer ", "Writer, Target"),
            Err(StorageError::InvalidCuration(_))
        ));
        assert_eq!(database.get_book(first).unwrap().unwrap(), before_collision);

        database
            .connection
            .execute_batch(&format!(
                "CREATE TEMP TRIGGER inject_contributor_merge_failure \
                 BEFORE DELETE ON contributors WHEN old.id = {} BEGIN \
                     SELECT raise(ABORT, 'injected contributor merge failure'); \
                 END;",
                source_contributor.value()
            ))
            .unwrap();
        assert!(
            database
                .merge_contributors(source_contributor, target_contributor)
                .is_err()
        );
        assert_eq!(
            database
                .contributor_mutation_impact(source_contributor)
                .unwrap()
                .books,
            2
        );
        assert_eq!(database.get_book(first).unwrap().unwrap(), before_collision);
        database
            .connection
            .execute_batch("DROP TRIGGER inject_contributor_merge_failure")
            .unwrap();

        database
            .merge_contributors(source_contributor, target_contributor)
            .unwrap();
        let merged_first = database.get_book(first).unwrap().unwrap();
        let first_authors = merged_first
            .contributors
            .iter()
            .filter(|credit| credit.role == ContributorRole::Author)
            .collect::<Vec<_>>();
        assert_eq!(first_authors.len(), 2);
        assert_eq!(first_authors[0].contributor.id, target_contributor);
        assert_eq!(first_authors[0].position, 0);
        assert_eq!(first_authors[1].position, 1);
        assert_eq!(merged_first.authors, "Target Writer, Other Writer");
        assert!(merged_first.contributors.iter().any(|credit| {
            credit.contributor.id == target_contributor && credit.role == ContributorRole::Editor
        }));
        assert!(
            database
                .search_contributors("Renamed Source", 0, 100)
                .unwrap()
                .is_empty()
        );
        assert!(database.delete_contributor(target_contributor).is_err());

        database
            .rename_series(source_series, "Renamed Series")
            .unwrap();
        database.merge_series(source_series, target_series).unwrap();
        for id in [first, second] {
            let membership = database
                .get_book(id)
                .unwrap()
                .unwrap()
                .series_membership
                .unwrap();
            assert_eq!(membership.series.id, target_series);
            assert_eq!(membership.series.name, "Target Series");
        }
        assert!(database.delete_series(target_series).is_err());

        database.rename_tag(source_tag, "Renamed Tag").unwrap();
        assert_eq!(
            database
                .query(&LibraryQuery {
                    search: "tag:\"Renamed Tag\"".into(),
                    ..LibraryQuery::default()
                })
                .unwrap()
                .len(),
            2
        );
        database.merge_tags(source_tag, target_tag).unwrap();
        for id in [first, second] {
            let tags = database.get_book(id).unwrap().unwrap().tags;
            assert_eq!(tags.len(), 1);
            assert_eq!(tags[0].id, target_tag);
        }

        let saved = database.list_saved_searches().unwrap();
        let source = saved.iter().find(|value| value.id == saved_source).unwrap();
        assert_eq!(source.query.facets.contributors.len(), 1);
        assert_eq!(
            source.query.facets.contributors[0].contributor,
            target_contributor
        );
        assert_eq!(source.query.facets.series, Some(target_series));
        assert_eq!(source.query.facets.included_tags, [target_tag]);
        let include = saved
            .iter()
            .find(|value| value.id == saved_target_wins)
            .unwrap();
        assert_eq!(include.query.facets.contributors.len(), 1);
        assert!(include.query.facets.contributors[0].author_only);
        assert_eq!(include.query.facets.included_tags, [target_tag]);
        assert!(include.query.facets.excluded_tags.is_empty());
        let exclude = saved
            .iter()
            .find(|value| value.id == saved_exclusion_wins)
            .unwrap();
        assert!(exclude.query.facets.included_tags.is_empty());
        assert_eq!(exclude.query.facets.excluded_tags, [target_tag]);

        let confirmed = database.tag_mutation_impact(target_tag).unwrap();
        assert!(
            database
                .delete_tag(
                    target_tag,
                    VocabularyMutationResult {
                        books: confirmed.books + 1,
                        ..confirmed
                    }
                )
                .is_err()
        );
        assert_eq!(database.tag_mutation_impact(target_tag).unwrap(), confirmed);
        assert_eq!(
            database.delete_tag(target_tag, confirmed).unwrap(),
            confirmed
        );
        assert!(
            database
                .search_tags("Target Tag", 0, 100)
                .unwrap()
                .is_empty()
        );
        assert!(
            database
                .query(&LibraryQuery {
                    search: "tag:\"Target Tag\"".into(),
                    ..LibraryQuery::default()
                })
                .unwrap()
                .is_empty()
        );
        for (offset, id) in [first, second].into_iter().enumerate() {
            let book = database.get_book(id).unwrap().unwrap();
            assert!(book.tags.is_empty());
            assert_eq!(book.assets, assets_before[offset]);
        }
        assert!(database.list_saved_searches().unwrap().iter().all(|saved| {
            !saved.query.facets.included_tags.contains(&target_tag)
                && !saved.query.facets.excluded_tags.contains(&target_tag)
        }));

        database
            .connection
            .execute(
                "INSERT INTO contributors(display_name, sort_name, identity_key, sort_key) \
                 VALUES ('Unused Writer', 'Unused Writer', 'unused writer', 'unused writer')",
                [],
            )
            .unwrap();
        let unused_contributor =
            lectern_core::organisation::ContributorId::new(database.connection.last_insert_rowid());
        database.delete_contributor(unused_contributor).unwrap();
        database
            .connection
            .execute(
                "INSERT INTO series_entities(name, identity_key) \
                 VALUES ('Unused Series', 'unused series')",
                [],
            )
            .unwrap();
        let unused_series =
            lectern_core::organisation::SeriesId::new(database.connection.last_insert_rowid());
        database.delete_series(unused_series).unwrap();
    }

    #[test]
    fn exact_facets_use_covering_relationship_indexes() {
        let database = LibraryDatabase::open_in_memory().expect("open library");
        for (sql, expected_index) in [
            (
                "SELECT 1 FROM book_contributors bc \
                 WHERE bc.contributor_id = 1 AND bc.role = 'author'",
                "book_contributors_contributor_role_book_idx",
            ),
            (
                "SELECT 1 FROM series_memberships sm \
                 WHERE sm.series_id = 1",
                "series_memberships_series_index_book_idx",
            ),
            (
                "SELECT 1 FROM book_tags bt WHERE bt.tag_id = 1",
                "book_tags_tag_book_idx",
            ),
        ] {
            let mut statement = database
                .connection
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .expect("prepare query plan");
            let details = statement
                .query_map([], |row| row.get::<_, String>(3))
                .expect("explain query")
                .collect::<rusqlite::Result<Vec<_>>>()
                .expect("collect query plan")
                .join("\n");
            assert!(details.contains(expected_index), "{details}");
            assert!(details.contains("COVERING"), "{details}");
        }
    }

    #[test]
    fn diagnostics_validate_database_index_relationships_and_files() {
        let asset = TestAsset::file("diagnostics-clean");
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        database
            .import_books(&[record(
                asset.path().to_string_lossy().as_ref(),
                "Dune",
                "Frank Herbert",
            )])
            .expect("import book");
        database
            .rescan_reference_assets()
            .expect("store file health");

        let report = database.diagnostics().expect("run diagnostics");

        assert!(report.is_healthy(), "{report:#?}");
        assert_eq!(report.schema_version, SCHEMA_VERSION);
        assert_eq!(report.referenced_files.checked, 1);
        assert_eq!(report.referenced_files.available, 1);
        assert_eq!(report.referenced_files.stale_health, 0);
    }

    #[test]
    fn diagnostics_report_missing_files_and_books_without_assets() {
        let asset = TestAsset::file("diagnostics-issues");
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let book = database
            .import_books(&[record(
                asset.path().to_string_lossy().as_ref(),
                "Dune",
                "Frank Herbert",
            )])
            .expect("import book")[0];
        database
            .rescan_reference_assets()
            .expect("store available health");
        fs::remove_file(asset.path()).expect("remove referenced file");
        database
            .connection
            .execute("DELETE FROM book_assets WHERE book_id = ?1", [book.value()])
            .expect("remove last asset outside service invariant");

        let report = database.diagnostics().expect("run diagnostics");

        assert!(!report.is_healthy());
        assert_eq!(report.books_without_assets, 1);
    }

    #[test]
    fn diagnostics_report_file_health_without_mutating_it() {
        let path = temporary_asset_path("diagnostics-missing");
        fs::write(&path, b"publication").expect("write referenced file");
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        database
            .import_books(&[record(
                path.to_string_lossy().as_ref(),
                "Dune",
                "Frank Herbert",
            )])
            .expect("import book");
        database
            .rescan_reference_assets()
            .expect("store available health");
        fs::remove_file(&path).expect("remove referenced file");

        let report = database.diagnostics().expect("run diagnostics");
        let book = database
            .get_book(BookId::new(1))
            .expect("load book")
            .expect("book exists");

        assert_eq!(report.referenced_files.missing, 1);
        assert_eq!(report.referenced_files.stale_health, 1);
        assert_eq!(book.assets[0].health, AssetHealth::Available);
    }

    #[test]
    fn backup_captures_committed_wal_data_and_refuses_overwrite() {
        let source = TestDatabase::new("backup-source");
        let destination = TestDatabase::new("backup-destination");
        let mut database = LibraryDatabase::open(source.path()).expect("open source library");
        database
            .import_books(&[record("/books/dune.epub", "Dune", "Frank Herbert")])
            .expect("import book into WAL database");

        let report = database.backup(destination.path()).expect("create backup");
        let backup = LibraryDatabase::open(destination.path()).expect("open backup");

        assert_eq!(report.destination, destination.path());
        assert!(report.bytes > 0);
        assert_eq!(report.books, 1);
        assert_eq!(backup.count().expect("count backup books"), 1);
        assert!(matches!(
            database.backup(destination.path()),
            Err(StorageError::BackupDestinationExists(path)) if path == destination.path()
        ));
        assert_eq!(backup.count().expect("backup remains readable"), 1);
    }

    #[test]
    fn stats_reconcile_book_asset_cover_and_health_counts() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let mut imported = record("/books/dune.epub", "Dune", "Frank Herbert");
        imported.cover_thumbnail = Some(vec![1, 2, 3]);
        database.import_books(&[imported]).expect("import book");

        let stats = database.stats().expect("load stats");

        assert_eq!(stats.books, 1);
        assert_eq!(stats.assets, 1);
        assert_eq!(stats.covers, 1);
        assert_eq!(stats.epub_assets, 1);
        assert_eq!(stats.referenced_assets, 1);
        assert_eq!(stats.unknown_assets, 1);
    }

    #[test]
    fn explicit_reimport_policy_can_replace_metadata() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let original = record("/books/dune.epub", "Edited title", "Edited author");
        let id = database.import_books(&[original]).expect("first import")[0];
        let mut replacement = record("/books/dune.epub", "Parsed title", "Parsed author");
        replacement.book.series = Some("Dune Saga".into());

        database
            .import_books_with_policy(&[replacement], ReimportMetadataPolicy::ReplaceExisting)
            .expect("replace metadata on reimport");
        let stored = database
            .get_book(id)
            .expect("load book")
            .expect("book exists");

        assert_eq!(stored.title, "Parsed title");
        assert_eq!(stored.authors, "Parsed author");
        assert_eq!(stored.contributors.len(), 1);
        assert_eq!(
            stored.contributors[0].contributor.display_name,
            "Parsed author"
        );
        assert_eq!(stored.series.as_deref(), Some("Dune Saga"));
        assert_eq!(
            stored
                .series_membership
                .as_ref()
                .map(|membership| membership.series.name.as_str()),
            Some("Dune Saga")
        );
    }

    #[test]
    fn saving_missing_book_is_reported() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let book = Book {
            id: BookId::new(404),
            title: "Missing".into(),
            authors: String::new(),
            series: None,
            contributors: Vec::new(),
            series_membership: None,
            tags: Vec::new(),
            identifiers: Vec::new(),
            publisher: None,
            language: None,
            description: None,
            assets: Vec::new(),
        };

        assert!(matches!(
            database.save_book(&book),
            Err(StorageError::BookNotFound(_))
        ));
    }
}
