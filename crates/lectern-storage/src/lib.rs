//! `SQLite` persistence adapter for Lectern.

use std::{
    ffi::OsString,
    fs::File,
    path::{Component, Path, PathBuf},
    time::Duration,
};

#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(windows)]
use std::os::windows::ffi::{OsStrExt, OsStringExt};

use lectern_core::{
    AssetHealth, AssetHealthReport, AssetId, AssetStorage, Book, BookAsset, BookAssetDraft,
    BookDraft, BookFormat, BookId, BookMetadataDraft, BookSummary, LibraryQuery, SortOrder,
};
use rusqlite::{
    Connection, OptionalExtension, Statement, Transaction, TransactionBehavior, params,
};
use thiserror::Error;

#[cfg(not(any(unix, windows)))]
compile_error!("Lectern's lossless path codec currently supports Unix and Windows targets");

const SCHEMA_VERSION: i64 = 4;

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
    /// An operation that requires a referenced asset was requested for a managed asset.
    #[error("asset {0} is managed and cannot be relinked as an external file")]
    AssetNotReference(AssetId),
    /// The replacement file did not match the asset's stored format.
    #[error("replacement format {found} does not match the expected {expected}")]
    RelinkFormatMismatch {
        /// Format stored for the asset being relinked.
        expected: BookFormat,
        /// Format validated from the replacement file.
        found: BookFormat,
    },
    /// The proposed replacement path was already linked by another reference asset.
    #[error("replacement path is already linked by asset {0}")]
    ReferencePathInUse(AssetId),
    /// The proposed replacement file was not usable at the time of relinking.
    #[error("replacement file is {0}")]
    ReplacementUnavailable(AssetHealth),
    /// The database returned an impossible negative row count.
    #[error("database returned invalid book count {0}")]
    InvalidCount(i64),
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
}

/// Result type returned by storage operations.
pub type Result<T> = std::result::Result<T, StorageError>;

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

/// Connection-scoped access to a Lectern library database.
pub struct LibraryDatabase {
    connection: Connection,
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
        Ok(Self { connection })
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

    /// Inserts or refreshes aggregate books in one transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when an aggregate is invalid, its existing paths resolve to different
    /// books, or the transaction cannot be committed.
    pub fn import_books(&mut self, records: &[BookImport]) -> Result<Vec<BookId>> {
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
                    )
                })
                .collect::<Result<Vec<_>>>()?
        };
        transaction.commit()?;
        Ok(ids)
    }

    /// Inserts or refreshes independent publication files in one transaction.
    ///
    /// This compatibility surface treats every record as a one-asset logical book. Aggregate
    /// importers should use [`Self::import_books`].
    ///
    /// # Errors
    ///
    /// Returns an error when the transaction cannot be started, a record cannot be written, or
    /// the transaction cannot be committed.
    pub fn import_batch(&mut self, records: &[ImportRecord]) -> Result<Vec<BookId>> {
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
                    )
                })
                .collect::<Result<Vec<_>>>()?
        };
        transaction.commit()?;
        Ok(ids)
    }

    /// Returns compact logical-book results matching `query`.
    ///
    /// # Errors
    ///
    /// Returns an error when the indexed query cannot be prepared or executed.
    pub fn query(&self, query: &LibraryQuery) -> Result<Vec<BookSummary>> {
        let search = build_fts_query(&query.search);
        let mut bindings = Vec::<rusqlite::types::Value>::new();
        let mut predicates = Vec::new();
        let mut joins = Vec::new();

        if let Some(search) = search {
            bindings.push(search.into());
            predicates.push(format!("books_fts MATCH ?{}", bindings.len()));
            joins.push("JOIN books_fts ON books_fts.rowid = b.id".to_owned());
        }

        if let Some(format) = query.format {
            bindings.push(format.as_str().to_owned().into());
            joins.push(format!(
                "JOIN book_assets filtered_assets \
                 ON filtered_assets.book_id = b.id AND filtered_assets.format = ?{}",
                bindings.len()
            ));
        }

        if let Some(health) = query.asset_health {
            bindings.push(health.as_str().to_owned().into());
            joins.push(format!(
                "JOIN (SELECT book_id FROM book_assets WHERE health = ?{} GROUP BY book_id) \
                 health_assets ON health_assets.book_id = b.id",
                bindings.len()
            ));
        }

        let where_clause = if predicates.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", predicates.join(" AND "))
        };
        let order = match query.sort {
            SortOrder::Title => "b.sort_title, b.id",
            SortOrder::Author => "b.sort_authors, b.sort_title, b.id",
            SortOrder::RecentlyAdded => "b.added_at DESC, b.id DESC",
        };
        let sql = format!(
            "SELECT b.id, b.title, b.authors, b.series, \
             EXISTS(SELECT 1 FROM book_covers c WHERE c.book_id = b.id), \
             EXISTS(SELECT 1 FROM book_assets a \
                    WHERE a.book_id = b.id AND a.health IN ('missing', 'unreadable')) \
             FROM books b {} {where_clause} ORDER BY {order}",
            joins.join(" ")
        );

        let mut statement = self.connection.prepare_cached(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(bindings), |row| {
            Ok(BookSummary {
                id: BookId::new(row.get(0)?),
                title: row.get(1)?,
                authors: row.get(2)?,
                series: row.get(3)?,
                has_cover: row.get(4)?,
                has_file_issue: row.get(5)?,
            })
        })?;

        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
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
    pub fn save_book(&self, book: &Book) -> Result<()> {
        let changed = self
            .connection
            .prepare_cached(
                "UPDATE books SET title = ?1, sort_title = ?2, authors = ?3, sort_authors = ?4, \
                 series = ?5, publisher = ?6, language = ?7, description = ?8, \
                 modified_at = unixepoch() WHERE id = ?9",
            )?
            .execute(params![
                book.title.trim(),
                sortable(&book.title),
                book.authors.trim(),
                sortable(&book.authors),
                optional_text(book.series.as_deref()),
                optional_text(book.publisher.as_deref()),
                optional_text(book.language.as_deref()),
                optional_text(book.description.as_deref()),
                book.id.value(),
            ])?;
        if changed == 0 {
            return Err(StorageError::BookNotFound(book.id));
        }
        Ok(())
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
        Ok(report)
    }

    /// Replaces the external path for a validated reference asset without changing its identity.
    ///
    /// Callers must validate the replacement publication before invoking this method. The supplied
    /// `replacement_format` is checked against the stored asset format inside the same immediate
    /// transaction that protects path uniqueness. On success, the asset retains its ID and its
    /// owning book, metadata, and cover are untouched.
    ///
    /// # Errors
    ///
    /// Returns an error when the asset is absent or managed, the validated format differs, the
    /// replacement is unavailable, or another reference asset already owns the replacement path.
    pub fn relink_reference_asset(
        &mut self,
        id: AssetId,
        replacement_path: impl AsRef<Path>,
        replacement_format: BookFormat,
    ) -> Result<()> {
        let replacement_path = replacement_path.as_ref();
        validate_asset_path(AssetStorage::Reference, replacement_path)?;
        let replacement_health = inspect_reference_asset(replacement_path);
        if replacement_health != AssetHealth::Available {
            return Err(StorageError::ReplacementUnavailable(replacement_health));
        }
        let (path_encoding, path) = encode_path(replacement_path);

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (format, storage) = transaction
            .query_row(
                "SELECT format, storage_mode FROM book_assets WHERE id = ?1",
                [id.value()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .ok_or(StorageError::AssetNotFound(id))?;
        let format = decode_format(&format)?;
        if format != replacement_format {
            return Err(StorageError::RelinkFormatMismatch {
                expected: format,
                found: replacement_format,
            });
        }
        if decode_storage(&storage)? != AssetStorage::Reference {
            return Err(StorageError::AssetNotReference(id));
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
        0..=3 => {}
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
            true
        }
        1 | 2 => {
            transaction.execute_batch(MIGRATE_1_OR_2_TO_3)?;
            transaction.execute_batch(MIGRATE_3_TO_4)?;
            true
        }
        3 => {
            transaction.execute_batch(MIGRATE_3_TO_4)?;
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
    update_book: Statement<'connection>,
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
            update_book: transaction.prepare(
                "UPDATE books SET \
                     title = ?1, sort_title = ?2, authors = ?3, sort_authors = ?4, \
                     series = ?5, publisher = ?6, language = ?7, description = ?8, \
                     modified_at = unixepoch() \
                 WHERE id = ?9",
            )?,
            upsert_asset: transaction.prepare(
                "INSERT INTO book_assets ( \
                     book_id, format, storage_mode, path_encoding, path \
                 ) VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(book_id, format) DO UPDATE SET \
                     storage_mode = excluded.storage_mode, \
                     path_encoding = excluded.path_encoding, \
                     path = excluded.path, \
                     modified_at = unixepoch()",
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
        statements.update_book.execute(params![
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

fn build_fts_query(input: &str) -> Option<String> {
    let terms = input
        .split_whitespace()
        .map(|term| term.replace('"', "\"\""))
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{term}\"*"))
        .collect::<Vec<_>>();
    (!terms.is_empty()).then(|| terms.join(" AND "))
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
        AssetHealth, AssetHealthReport, AssetStorage, Book, BookAssetDraft, BookFormat, BookId,
        BookMetadataDraft, LibraryQuery,
    };
    use rusqlite::Connection;

    use super::{
        BookImport, ImportRecord, LibraryDatabase, SCHEMA, SCHEMA_VERSION, StorageError,
        configure_persistent_database, decode_path,
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
    fn migrates_version_three_libraries_with_unknown_asset_health() {
        let database_file = TestDatabase::new("migration-v3");
        let version_three_schema = SCHEMA
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
    fn reimport_by_one_asset_preserves_book_assets_and_cover() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let mut first = BookImport {
            book: metadata("Dune", "Frank Herbert"),
            assets: vec![
                asset("/books/dune.epub", BookFormat::Epub),
                asset("/books/dune.pdf", BookFormat::Pdf),
            ],
            cover_thumbnail: Some(vec![1, 2, 3]),
        };
        let id = database
            .import_books(std::slice::from_ref(&first))
            .expect("first import")[0];
        let original_assets = database
            .get_book(id)
            .expect("load original")
            .expect("original exists")
            .assets;

        first.book.title = "Dune: Deluxe Edition".into();
        first.assets.truncate(1);
        first.cover_thumbnail = None;
        let replacement_id = database.import_books(&[first]).expect("second import")[0];
        let updated = database
            .get_book(id)
            .expect("load updated")
            .expect("updated exists");

        assert_eq!(id, replacement_id);
        assert_eq!(updated.title, "Dune: Deluxe Edition");
        assert_eq!(updated.assets, original_assets);
        assert_eq!(
            database.load_cover(id).expect("load cover"),
            Some(vec![1, 2, 3])
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
    fn deleting_a_book_cascades_to_assets_and_cover() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let mut imported = record("/books/dune.epub", "Dune", "Frank Herbert");
        imported.cover_thumbnail = Some(vec![1, 2, 3]);
        let id = database.import_books(&[imported]).expect("import book")[0];

        database
            .connection
            .execute("DELETE FROM books WHERE id = ?1", [id.value()])
            .expect("delete book");

        let assets: i64 = database
            .connection
            .query_row("SELECT count(*) FROM book_assets", [], |row| row.get(0))
            .expect("count assets");
        assert_eq!(assets, 0);
        assert_eq!(database.load_cover(id).expect("load cover"), None);
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

    #[test]
    fn saving_missing_book_is_reported() {
        let database = LibraryDatabase::open_in_memory().expect("open library");
        let book = Book {
            id: BookId::new(404),
            title: "Missing".into(),
            authors: String::new(),
            series: None,
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
