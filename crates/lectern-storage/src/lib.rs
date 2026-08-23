//! `SQLite` persistence adapter for Lectern.

use std::{path::Path, time::Duration};

use lectern_core::{Book, BookDraft, BookFormat, BookId, BookSummary, LibraryQuery, SortOrder};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use thiserror::Error;

const SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = r"
CREATE TABLE books (
    id          INTEGER PRIMARY KEY,
    title       TEXT NOT NULL,
    sort_title  TEXT NOT NULL,
    authors     TEXT NOT NULL,
    sort_authors TEXT NOT NULL,
    series      TEXT,
    publisher   TEXT,
    language    TEXT,
    description TEXT,
    format      TEXT NOT NULL CHECK (format IN ('epub', 'pdf')),
    source_path TEXT NOT NULL UNIQUE,
    added_at    INTEGER NOT NULL DEFAULT (unixepoch()),
    modified_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX books_sort_title_idx ON books(sort_title, id);
CREATE INDEX books_sort_authors_idx ON books(sort_authors, sort_title, id);
CREATE INDEX books_added_at_idx ON books(added_at DESC, id DESC);
CREATE INDEX books_format_title_idx ON books(format, sort_title, id);

CREATE TABLE book_covers (
    book_id INTEGER PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
    jpeg    BLOB NOT NULL
);

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

CREATE TRIGGER books_after_update AFTER UPDATE ON books BEGIN
    INSERT INTO books_fts(books_fts, rowid, title, authors, series, publisher)
    VALUES ('delete', old.id, old.title, old.authors, old.series, old.publisher);
    INSERT INTO books_fts(rowid, title, authors, series, publisher)
    VALUES (new.id, new.title, new.authors, new.series, new.publisher);
END;

PRAGMA user_version = 2;
";

const MIGRATE_1_TO_2: &str = r"
BEGIN IMMEDIATE;

DROP TRIGGER books_after_insert;
DROP TRIGGER books_after_delete;
DROP TRIGGER books_after_update;
DROP TABLE books_fts;
DROP INDEX books_sort_title_idx;
DROP INDEX books_sort_authors_idx;
DROP INDEX books_added_at_idx;
DROP INDEX books_format_title_idx;

ALTER TABLE book_covers RENAME TO book_covers_v1;
ALTER TABLE books RENAME TO books_v1;

CREATE TABLE books (
    id          INTEGER PRIMARY KEY,
    title       TEXT NOT NULL,
    sort_title  TEXT NOT NULL,
    authors     TEXT NOT NULL,
    sort_authors TEXT NOT NULL,
    series      TEXT,
    publisher   TEXT,
    language    TEXT,
    description TEXT,
    format      TEXT NOT NULL CHECK (format IN ('epub', 'pdf')),
    source_path TEXT NOT NULL UNIQUE,
    added_at    INTEGER NOT NULL DEFAULT (unixepoch()),
    modified_at INTEGER NOT NULL DEFAULT (unixepoch())
);

INSERT INTO books (
    id, title, sort_title, authors, sort_authors, series, publisher, language, description,
    format, source_path, added_at, modified_at
)
SELECT
    id, title, sort_title, authors, sort_authors, series, publisher, language, description,
    format, source_path, added_at, modified_at
FROM books_v1;

CREATE INDEX books_sort_title_idx ON books(sort_title, id);
CREATE INDEX books_sort_authors_idx ON books(sort_authors, sort_title, id);
CREATE INDEX books_added_at_idx ON books(added_at DESC, id DESC);
CREATE INDEX books_format_title_idx ON books(format, sort_title, id);

CREATE TABLE book_covers (
    book_id INTEGER PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
    jpeg    BLOB NOT NULL
);

INSERT INTO book_covers(book_id, jpeg)
SELECT book_id, jpeg FROM book_covers_v1;

DROP TABLE book_covers_v1;
DROP TABLE books_v1;

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

CREATE TRIGGER books_after_update AFTER UPDATE ON books BEGIN
    INSERT INTO books_fts(books_fts, rowid, title, authors, series, publisher)
    VALUES ('delete', old.id, old.title, old.authors, old.series, old.publisher);
    INSERT INTO books_fts(rowid, title, authors, series, publisher)
    VALUES (new.id, new.title, new.authors, new.series, new.publisher);
END;

INSERT INTO books_fts(books_fts) VALUES ('rebuild');
PRAGMA user_version = 2;

COMMIT;
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
    /// The database returned an impossible negative row count.
    #[error("database returned invalid book count {0}")]
    InvalidCount(i64),
}

/// Result type returned by storage operations.
pub type Result<T> = std::result::Result<T, StorageError>;

/// Parsed book and optional encoded thumbnail ready for transactional import.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportRecord {
    /// Parsed metadata and source path.
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

    fn from_connection(connection: Connection, persistent: bool) -> Result<Self> {
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA temp_store = MEMORY;")?;
        if persistent {
            connection.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
        }

        let version = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        match version {
            0 => connection.execute_batch(SCHEMA)?,
            1 => migrate_from_version_one(&connection)?,
            SCHEMA_VERSION => {}
            unsupported => return Err(StorageError::UnsupportedSchema(unsupported)),
        }

        Ok(Self { connection })
    }

    /// Returns the number of books in the library.
    ///
    /// # Errors
    ///
    /// Returns an error when the count query fails or returns an invalid value.
    pub fn count(&self) -> Result<u64> {
        let count: i64 = self
            .connection
            .query_row("SELECT count(*) FROM books", [], |row| row.get(0))?;
        u64::try_from(count).map_err(|_| StorageError::InvalidCount(count))
    }

    /// Inserts or refreshes a batch in one transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when the transaction cannot be started, a record cannot be written, or
    /// the transaction cannot be committed.
    pub fn import_batch(&mut self, records: &[ImportRecord]) -> Result<Vec<BookId>> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids = records
            .iter()
            .map(|record| upsert_record(&transaction, record))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        transaction.commit()?;
        Ok(ids)
    }

    /// Returns compact results matching `query`.
    ///
    /// # Errors
    ///
    /// Returns an error when the indexed query cannot be prepared or executed.
    pub fn query(&self, query: &LibraryQuery) -> Result<Vec<BookSummary>> {
        let search = build_fts_query(&query.search);
        let mut bindings = Vec::<rusqlite::types::Value>::new();
        let mut predicates = Vec::new();

        let join = if let Some(search) = search {
            bindings.push(search.into());
            predicates.push(format!("books_fts MATCH ?{}", bindings.len()));
            "JOIN books_fts ON books_fts.rowid = b.id"
        } else {
            ""
        };

        if let Some(format) = query.format {
            bindings.push(format.as_str().to_owned().into());
            predicates.push(format!("b.format = ?{}", bindings.len()));
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
            "SELECT b.id, b.title, b.authors, b.series, b.format, \
             EXISTS(SELECT 1 FROM book_covers c WHERE c.book_id = b.id) \
             FROM books b {join} {where_clause} ORDER BY {order}"
        );

        let mut statement = self.connection.prepare_cached(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(bindings), |row| {
            let format = row.get::<_, String>(4)?;
            Ok(BookSummary {
                id: BookId::new(row.get(0)?),
                title: row.get(1)?,
                authors: row.get(2)?,
                series: row.get(3)?,
                format: decode_format(&format),
                has_cover: row.get(5)?,
            })
        })?;

        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Loads complete editable metadata for one book.
    ///
    /// # Errors
    ///
    /// Returns an error when the book query cannot be executed or decoded.
    pub fn get_book(&self, id: BookId) -> Result<Option<Book>> {
        let book = self
            .connection
            .query_row(
                "SELECT id, title, authors, series, publisher, language, description, format, \
                 source_path FROM books WHERE id = ?1",
                [id.value()],
                |row| {
                    let format = row.get::<_, String>(7)?;
                    Ok(Book {
                        id: BookId::new(row.get(0)?),
                        title: row.get(1)?,
                        authors: row.get(2)?,
                        series: row.get(3)?,
                        publisher: row.get(4)?,
                        language: row.get(5)?,
                        description: row.get(6)?,
                        format: decode_format(&format),
                        source_path: row.get::<_, String>(8)?.into(),
                    })
                },
            )
            .optional()?;
        Ok(book)
    }

    /// Persists editable metadata for an existing book.
    ///
    /// # Errors
    ///
    /// Returns an error when the update fails or the book no longer exists.
    pub fn save_book(&self, book: &Book) -> Result<()> {
        let changed = self.connection.execute(
            "UPDATE books SET title = ?1, sort_title = ?2, authors = ?3, sort_authors = ?4, \
             series = ?5, publisher = ?6, language = ?7, description = ?8, \
             modified_at = unixepoch() WHERE id = ?9",
            params![
                book.title.trim(),
                sortable(&book.title),
                book.authors.trim(),
                sortable(&book.authors),
                optional_text(book.series.as_deref()),
                optional_text(book.publisher.as_deref()),
                optional_text(book.language.as_deref()),
                optional_text(book.description.as_deref()),
                book.id.value(),
            ],
        )?;
        if changed == 0 {
            return Err(StorageError::BookNotFound(book.id));
        }
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
            .query_row(
                "SELECT jpeg FROM book_covers WHERE book_id = ?1",
                [id.value()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(cover)
    }
}

fn migrate_from_version_one(connection: &Connection) -> Result<()> {
    connection.execute_batch("PRAGMA foreign_keys = OFF;")?;
    let migration = connection.execute_batch(MIGRATE_1_TO_2);
    let restore_foreign_keys = connection.execute_batch("PRAGMA foreign_keys = ON;");
    migration?;
    restore_foreign_keys?;
    Ok(())
}

fn upsert_record(transaction: &Transaction<'_>, record: &ImportRecord) -> rusqlite::Result<BookId> {
    let book = &record.book;
    let source_path = book.source_path.to_string_lossy();
    let id = transaction.query_row(
        "INSERT INTO books (
             title, sort_title, authors, sort_authors, series, publisher, language, description,
             format, source_path
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(source_path) DO UPDATE SET
             title = excluded.title,
             sort_title = excluded.sort_title,
             authors = excluded.authors,
             sort_authors = excluded.sort_authors,
             series = excluded.series,
             publisher = excluded.publisher,
             language = excluded.language,
             description = excluded.description,
             format = excluded.format,
             modified_at = unixepoch()
         RETURNING id",
        params![
            book.title.trim(),
            sortable(&book.title),
            book.authors.trim(),
            sortable(&book.authors),
            optional_text(book.series.as_deref()),
            optional_text(book.publisher.as_deref()),
            optional_text(book.language.as_deref()),
            optional_text(book.description.as_deref()),
            book.format.as_str(),
            source_path,
        ],
        |row| row.get(0),
    )?;

    if let Some(cover) = &record.cover_thumbnail {
        transaction.execute(
            "INSERT INTO book_covers(book_id, jpeg) VALUES (?1, ?2) \
             ON CONFLICT(book_id) DO UPDATE SET jpeg = excluded.jpeg",
            params![id, cover],
        )?;
    }

    Ok(BookId::new(id))
}

fn optional_text(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn sortable(value: &str) -> String {
    value.trim().to_lowercase()
}

fn decode_format(value: &str) -> BookFormat {
    BookFormat::parse(value).unwrap_or(BookFormat::Epub)
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

    use lectern_core::{BookDraft, BookFormat, LibraryQuery, SortOrder};
    use rusqlite::Connection;

    use super::{ImportRecord, LibraryDatabase, SCHEMA, SCHEMA_VERSION, StorageError};

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
            if self.0.exists() {
                fs::remove_file(&self.0).expect("remove test database");
            }
        }
    }

    fn record(path: &str, title: &str, authors: &str) -> ImportRecord {
        record_with_format(path, title, authors, BookFormat::Epub)
    }

    fn record_with_format(
        path: &str,
        title: &str,
        authors: &str,
        format: BookFormat,
    ) -> ImportRecord {
        ImportRecord {
            book: BookDraft {
                title: title.into(),
                authors: authors.into(),
                series: None,
                publisher: None,
                language: Some("en".into()),
                description: None,
                format,
                source_path: PathBuf::from(path),
            },
            cover_thumbnail: None,
        }
    }

    #[test]
    fn stores_and_filters_pdf_books() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        database
            .import_batch(&[
                record("/books/dune.epub", "Dune", "Frank Herbert"),
                record_with_format(
                    "/books/manual.pdf",
                    "Field Manual",
                    "Octavia Butler",
                    BookFormat::Pdf,
                ),
            ])
            .expect("import books");

        let results = database
            .query(&LibraryQuery {
                format: Some(BookFormat::Pdf),
                ..LibraryQuery::default()
            })
            .expect("query PDFs");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Field Manual");
        assert_eq!(results[0].format, BookFormat::Pdf);
    }

    #[test]
    fn migrates_version_one_libraries_without_losing_data() {
        let database_file = TestDatabase::new("migration");
        let version_one_schema = SCHEMA
            .replace(
                "CHECK (format IN ('epub', 'pdf'))",
                "CHECK (format IN ('epub'))",
            )
            .replace("PRAGMA user_version = 2", "PRAGMA user_version = 1");
        let connection = Connection::open(database_file.path()).expect("open v1 database");
        connection
            .execute_batch(&version_one_schema)
            .expect("create v1 schema");
        connection
            .execute(
                "INSERT INTO books (
                    id, title, sort_title, authors, sort_authors, format, source_path
                 ) VALUES (7, 'Dune', 'dune', 'Frank Herbert', 'frank herbert', 'epub',
                           '/books/dune.epub')",
                [],
            )
            .expect("insert v1 book");
        connection
            .execute(
                "INSERT INTO book_covers(book_id, jpeg) VALUES (7, x'010203')",
                [],
            )
            .expect("insert v1 cover");
        drop(connection);

        let mut database = LibraryDatabase::open(database_file.path()).expect("migrate database");

        let version: i64 = database
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("read schema version");
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(database.count().expect("count migrated books"), 1);
        assert_eq!(
            database
                .load_cover(lectern_core::BookId::new(7))
                .expect("load migrated cover"),
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

        database
            .import_batch(&[record_with_format(
                "/books/manual.pdf",
                "Field Manual",
                "Octavia Butler",
                BookFormat::Pdf,
            )])
            .expect("insert PDF after migration");
        assert_eq!(database.count().expect("count books"), 2);
    }

    #[test]
    fn imports_and_queries_books() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        database
            .import_batch(&[
                record("/books/dune.epub", "Dune", "Frank Herbert"),
                record(
                    "/books/earthsea.epub",
                    "A Wizard of Earthsea",
                    "Ursula K. Le Guin",
                ),
            ])
            .expect("import books");

        assert_eq!(database.count().expect("count books"), 2);

        let results = database
            .query(&LibraryQuery {
                search: "wiz earth".into(),
                format: Some(BookFormat::Epub),
                sort: SortOrder::Title,
            })
            .expect("query books");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "A Wizard of Earthsea");
    }

    #[test]
    fn reimport_updates_in_place_and_preserves_cover() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let mut first = record("/books/dune.epub", "Dune", "Frank Herbert");
        first.cover_thumbnail = Some(vec![1, 2, 3]);
        let id = database.import_batch(&[first]).expect("first import")[0];

        let replacement = record("/books/dune.epub", "Dune: Deluxe Edition", "Frank Herbert");
        let replacement_id = database
            .import_batch(&[replacement])
            .expect("second import")[0];

        assert_eq!(id, replacement_id);
        assert_eq!(database.count().expect("count books"), 1);
        assert_eq!(
            database.load_cover(id).expect("load cover"),
            Some(vec![1, 2, 3])
        );
    }

    #[test]
    fn metadata_updates_refresh_search_index() {
        let mut database = LibraryDatabase::open_in_memory().expect("open library");
        let id = database
            .import_batch(&[record("/books/dune.epub", "Dune", "Frank Herbert")])
            .expect("import book")[0];
        let mut book = database
            .get_book(id)
            .expect("load book")
            .expect("book exists");
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
    }

    #[test]
    fn saving_missing_book_is_reported() {
        let database = LibraryDatabase::open_in_memory().expect("open library");
        let mut book = lectern_core::Book {
            id: lectern_core::BookId::new(404),
            title: "Missing".into(),
            authors: String::new(),
            series: None,
            publisher: None,
            language: None,
            description: None,
            format: BookFormat::Epub,
            source_path: "/missing.epub".into(),
        };
        book.title.push('!');

        assert!(matches!(
            database.save_book(&book),
            Err(StorageError::BookNotFound(_))
        ));
    }
}
