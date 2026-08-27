//! Deterministic normalized-organisation storage workloads.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use lectern_storage::LibraryDatabase;
use rusqlite::{Connection, TransactionBehavior, params};
use serde::Serialize;

const USAGE: &str = "Usage:
  organisation-benchmark seed-migration --database PATH --output PATH [OPTIONS]
  organisation-benchmark migration --database PATH --output PATH [OPTIONS]

Options:
  --books N          Logical books in the version-five fixture (default: 50000)
  --seed N           Deterministic fixture seed (default: 20260825)
  --cover-every N    Give every Nth book a cover; zero disables covers (default: 3)
  --iterations N     Measured independent migrations (default: 20)
  --warmup N         Warmup independent migrations (default: 2)
";

const VERSION_FIVE_SCHEMA: &str = r"
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
    ON book_assets(path_encoding, path) WHERE storage_mode = 'reference';
CREATE INDEX book_assets_format_book_idx ON book_assets(format, book_id);
CREATE INDEX book_assets_health_book_idx ON book_assets(health, book_id);

CREATE TABLE book_covers (
    book_id INTEGER PRIMARY KEY REFERENCES books(id) ON DELETE CASCADE,
    jpeg BLOB NOT NULL
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
    UPDATE books SET has_file_issue = EXISTS(
        SELECT 1 FROM book_assets
        WHERE book_id = new.book_id AND health IN ('missing', 'unreadable')
    ) WHERE id = new.book_id;
END;
CREATE TRIGGER book_assets_after_delete_summary
AFTER DELETE ON book_assets
BEGIN
    UPDATE books SET has_file_issue = EXISTS(
        SELECT 1 FROM book_assets
        WHERE book_id = old.book_id AND health IN ('missing', 'unreadable')
    ) WHERE id = old.book_id;
END;

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

#[derive(Clone, Debug)]
struct Options {
    database: PathBuf,
    output: PathBuf,
    books: u64,
    seed: u64,
    cover_every: u64,
    iterations: usize,
    warmup: usize,
}

impl Options {
    fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<(String, Self), String> {
        let mut arguments = arguments.into_iter();
        let command = arguments
            .next()
            .and_then(|value| value.into_string().ok())
            .ok_or_else(|| USAGE.to_owned())?;
        if matches!(command.as_str(), "help" | "--help" | "-h") {
            return Err(USAGE.to_owned());
        }
        let mut options = Self {
            database: PathBuf::new(),
            output: PathBuf::new(),
            books: 50_000,
            seed: 20_260_825,
            cover_every: 3,
            iterations: 20,
            warmup: 2,
        };
        while let Some(argument) = arguments.next() {
            let name = argument
                .into_string()
                .map_err(|_| "option names must be UTF-8".to_owned())?;
            let value = arguments
                .next()
                .ok_or_else(|| format!("missing value for {name}"))?;
            match name.as_str() {
                "--database" => options.database = PathBuf::from(value),
                "--output" => options.output = PathBuf::from(value),
                "--books" => options.books = parse_number(&name, value)?,
                "--seed" => options.seed = parse_number(&name, value)?,
                "--cover-every" => options.cover_every = parse_number(&name, value)?,
                "--iterations" => options.iterations = parse_number(&name, value)?,
                "--warmup" => options.warmup = parse_number(&name, value)?,
                _ => return Err(format!("unknown option {name:?}")),
            }
        }
        if !matches!(command.as_str(), "seed-migration" | "migration") {
            return Err(format!("unknown command {command:?}"));
        }
        if options.database.as_os_str().is_empty() || options.output.as_os_str().is_empty() {
            return Err("--database and --output are required".into());
        }
        if options.database == options.output {
            return Err("--database and --output must be distinct".into());
        }
        if options.books == 0 || options.iterations == 0 {
            return Err("--books and --iterations must be greater than zero".into());
        }
        Ok((command, options))
    }
}

fn parse_number<T>(name: &str, value: OsString) -> Result<T, String>
where
    T: std::str::FromStr,
{
    value
        .into_string()
        .map_err(|_| format!("{name} must be UTF-8"))?
        .parse()
        .map_err(|_| format!("invalid numeric value for {name}"))
}

fn main() {
    match Options::parse(std::env::args_os().skip(1)).and_then(|(command, options)| {
        if command == "seed-migration" {
            seed_migration(&options)
        } else {
            run_migration(&options)
        }
    }) {
        Ok(()) => {}
        Err(error) if error == USAGE => println!("{USAGE}"),
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(2);
        }
    }
}

#[derive(Serialize)]
struct MigrationSeedResult {
    schema_version: u32,
    kind: &'static str,
    database_path: String,
    library_books: u64,
    contributor_vocabulary: u64,
    series_vocabulary: u64,
    covers: u64,
    seed: u64,
    database_bytes: u64,
    elapsed_ms: f64,
}

#[allow(clippy::too_many_lines)]
fn seed_migration(options: &Options) -> Result<(), String> {
    ensure_new_file(&options.database)?;
    ensure_new_file(&options.output)?;
    create_parent(&options.database)?;
    create_parent(&options.output)?;

    let started = Instant::now();
    let mut connection = Connection::open(&options.database).map_err(display_error)?;
    connection
        .execute_batch(VERSION_FIVE_SCHEMA)
        .map_err(display_error)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(display_error)?;
    {
        let mut insert_book = transaction
            .prepare(
                "INSERT INTO books( \
                     id, title, sort_title, authors, sort_authors, series, \
                     publisher, language, description, added_at, modified_at \
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'en', ?8, ?9, ?9)",
            )
            .map_err(display_error)?;
        let mut insert_asset = transaction
            .prepare(
                "INSERT INTO book_assets( \
                     id, book_id, format, storage_mode, health, path_encoding, path, \
                     added_at, modified_at \
                 ) VALUES (?1, ?1, ?2, 'reference', ?3, 'utf8', ?4, ?5, ?5)",
            )
            .map_err(display_error)?;
        let mut insert_cover = transaction
            .prepare("INSERT INTO book_covers(book_id, jpeg) VALUES (?1, ?2)")
            .map_err(display_error)?;
        let cover = representative_cover();

        for offset in 0..options.books {
            let id = i64::try_from(offset + 1).map_err(display_error)?;
            let title = format!("Title {offset:05}");
            let author_number = mixed_index(offset, options.seed, 20_000);
            let authors = format!("Contributor {author_number:05}");
            let series = (offset % 10 < 7).then(|| {
                let membership_ordinal = (offset / 10) * 7 + offset % 10;
                let number = mixed_index(membership_ordinal, options.seed ^ 0x51_7e, 2_500);
                format!("Series {number:04}")
            });
            let publisher = format!("Publisher {:03}", offset % 200);
            let description = format!("Deterministic migration fixture {offset}");
            let timestamp =
                1_700_000_000_i64 + i64::try_from(offset % 1_000_000).map_err(display_error)?;
            insert_book
                .execute(params![
                    id,
                    title,
                    title.to_ascii_lowercase(),
                    authors,
                    authors.to_ascii_lowercase(),
                    series,
                    publisher,
                    description,
                    timestamp,
                ])
                .map_err(display_error)?;

            let format = if offset % 10 < 7 { "epub" } else { "pdf" };
            let health = match offset % 20 {
                0 => "missing",
                1 => "unreadable",
                2..=7 => "available",
                _ => "unknown",
            };
            let path = format!("/benchmark/library/{id}.{format}").into_bytes();
            insert_asset
                .execute(params![id, format, health, path, timestamp])
                .map_err(display_error)?;
            if options.cover_every != 0 && offset % options.cover_every == 0 {
                insert_cover
                    .execute(params![id, cover])
                    .map_err(display_error)?;
            }
        }
    }
    transaction
        .pragma_update(None, "user_version", 5)
        .map_err(display_error)?;
    transaction.commit().map_err(display_error)?;
    connection
        .execute("INSERT INTO books_fts(books_fts) VALUES ('optimize')", [])
        .map_err(display_error)?;
    drop(connection);

    let result = MigrationSeedResult {
        schema_version: 1,
        kind: "organisation-migration-seed",
        database_path: options.database.display().to_string(),
        library_books: options.books,
        contributor_vocabulary: options.books.min(20_000),
        series_vocabulary: expected_series_count(options.books),
        covers: expected_cover_count(options.books, options.cover_every),
        seed: options.seed,
        database_bytes: fs::metadata(&options.database)
            .map_err(display_error)?
            .len(),
        elapsed_ms: started.elapsed().as_secs_f64() * 1_000.0,
    };
    write_json(&options.output, &result)?;
    println!(
        "Seeded version-five migration library with {} books in {:.1} ms",
        options.books, result.elapsed_ms
    );
    Ok(())
}

fn mixed_index(value: u64, seed: u64, modulus: u64) -> u64 {
    value.wrapping_mul(7_919).wrapping_add(seed) % modulus
}

fn representative_cover() -> Vec<u8> {
    let mut bytes = vec![0_u8; 8 * 1024];
    bytes[0..4].copy_from_slice(&[0xff, 0xd8, 0xff, 0xe0]);
    let length = bytes.len();
    bytes[length - 2..].copy_from_slice(&[0xff, 0xd9]);
    for (index, byte) in bytes[4..length - 2].iter_mut().enumerate() {
        *byte = u8::try_from(index % 251).expect("modulo is byte-sized");
    }
    bytes
}

#[derive(Serialize)]
#[allow(clippy::struct_excessive_bools)]
struct MigrationResult {
    schema_version: u32,
    kind: &'static str,
    measured_at_unix_ms: u128,
    database_path: String,
    source_schema_version: u32,
    final_schema_version: u32,
    library_books: u64,
    warmup_iterations: usize,
    measured_iterations: usize,
    visible_projections_preserved: bool,
    book_asset_cover_identities_preserved: bool,
    fts_equivalent: bool,
    initial_tags_and_saved_searches_empty: bool,
    schema_invariants_valid: bool,
    canonical_metadata_defaults_valid: bool,
    duplicate_series_numbers_repaired: bool,
    failed_migration_rolled_back: bool,
    scenarios: Vec<MigrationScenario>,
}

#[derive(Serialize)]
struct MigrationScenario {
    name: &'static str,
    successful_migrations: usize,
    latency_ms: LatencySummary,
    samples_ns: Vec<u64>,
    peak_rss_bytes: u64,
}

#[derive(Serialize)]
struct LatencySummary {
    minimum: f64,
    p50: f64,
    p95: f64,
    p99: f64,
    maximum: f64,
}

fn run_migration(options: &Options) -> Result<(), String> {
    if !options.database.is_file() {
        return Err(format!(
            "version-five migration database is not a file: {}",
            options.database.display()
        ));
    }
    ensure_new_file(&options.output)?;
    create_parent(&options.output)?;
    validate_source_template(&options.database, options.books)?;
    let rollback_valid = validate_failed_migration_rollback(options)?;
    let version_seven = version_seven_template_path(&options.output);
    prepare_version_seven_template(&options.database, &version_seven, options)?;

    let rounds = options
        .warmup
        .checked_add(options.iterations)
        .ok_or_else(|| "migration iteration count overflowed".to_owned())?;
    let sampler = MemorySampler::start()?;
    let mut version_five_samples_ns = Vec::with_capacity(options.iterations);
    for round in 0..rounds {
        let candidate = candidate_path(&options.output, round);
        copy_database(&options.database, &candidate)?;
        let started = Instant::now();
        let database = LibraryDatabase::open(&candidate).map_err(display_error)?;
        let elapsed = started.elapsed();
        drop(database);
        validate_migrated_candidate(&candidate, options)?;
        remove_database_files(&candidate)?;
        if round >= options.warmup {
            version_five_samples_ns.push(duration_ns(elapsed)?);
        }
    }
    let mut version_seven_samples_ns = Vec::with_capacity(options.iterations);
    for round in 0..rounds {
        let candidate = candidate_path(&options.output, round);
        copy_database(&version_seven, &candidate)?;
        let started = Instant::now();
        let database = LibraryDatabase::open(&candidate).map_err(display_error)?;
        let elapsed = started.elapsed();
        drop(database);
        validate_migrated_candidate(&candidate, options)?;
        validate_repaired_series_numbers(&candidate, options)?;
        remove_database_files(&candidate)?;
        if round >= options.warmup {
            version_seven_samples_ns.push(duration_ns(elapsed)?);
        }
    }
    let peak_rss_bytes = sampler.finish()?;
    validate_source_template(&options.database, options.books)?;
    validate_version_seven_template(&version_seven, options)?;
    remove_database_files(&version_seven)?;

    let result = MigrationResult {
        schema_version: 1,
        kind: "organisation-migration",
        measured_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(display_error)?
            .as_millis(),
        database_path: options.database.display().to_string(),
        source_schema_version: 5,
        final_schema_version: 9,
        library_books: options.books,
        warmup_iterations: options.warmup,
        measured_iterations: options.iterations,
        visible_projections_preserved: true,
        book_asset_cover_identities_preserved: true,
        fts_equivalent: true,
        initial_tags_and_saved_searches_empty: true,
        schema_invariants_valid: true,
        canonical_metadata_defaults_valid: true,
        duplicate_series_numbers_repaired: true,
        failed_migration_rolled_back: rollback_valid,
        scenarios: vec![
            MigrationScenario {
                name: "migrate_version_five_library",
                successful_migrations: rounds,
                latency_ms: summarize_latency(&version_five_samples_ns),
                samples_ns: version_five_samples_ns,
                peak_rss_bytes,
            },
            MigrationScenario {
                name: "repair_version_seven_series_numbers",
                successful_migrations: rounds,
                latency_ms: summarize_latency(&version_seven_samples_ns),
                samples_ns: version_seven_samples_ns,
                peak_rss_bytes,
            },
        ],
    };
    write_json(&options.output, &result)?;
    println!(
        "Measured {} independent version-five and version-seven migrations over {} books",
        options.iterations, options.books
    );
    Ok(())
}

fn prepare_version_seven_template(
    version_five: &Path,
    destination: &Path,
    options: &Options,
) -> Result<(), String> {
    copy_database(version_five, destination)?;
    drop(LibraryDatabase::open(destination).map_err(display_error)?);
    let connection = Connection::open(destination).map_err(display_error)?;
    connection
        .execute_batch(
            "DROP INDEX series_memberships_series_number_uidx; \
             UPDATE series_memberships SET series_index = 1000000; \
             UPDATE books SET series_index = 1000000 WHERE series IS NOT NULL; \
             DROP TABLE book_metadata; \
             PRAGMA user_version = 7;",
        )
        .map_err(display_error)?;
    let journal_mode = connection
        .query_row("PRAGMA journal_mode = DELETE", [], |row| {
            row.get::<_, String>(0)
        })
        .map_err(display_error)?;
    if !journal_mode.eq_ignore_ascii_case("delete") {
        return Err(format!(
            "version-seven template could not consolidate its journal: {journal_mode}"
        ));
    }
    drop(connection);
    validate_version_seven_template(destination, options)
}

fn validate_version_seven_template(path: &Path, options: &Options) -> Result<(), String> {
    let connection = Connection::open(path).map_err(display_error)?;
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(display_error)?;
    if version != 7 {
        return Err(format!(
            "series-repair source schema is {version}, expected 7"
        ));
    }
    let future_metadata_tables: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_schema \
             WHERE type = 'table' AND name = 'book_metadata'",
            [],
            |row| row.get(0),
        )
        .map_err(display_error)?;
    if future_metadata_tables != 0 {
        return Err(
            "version-seven repair fixture contains publication metadata from schema version nine"
                .into(),
        );
    }
    expect_count(&connection, "books", options.books)?;
    let duplicates: bool = connection
        .query_row(
            "SELECT EXISTS( \
                 SELECT 1 FROM series_memberships \
                 GROUP BY series_id, series_index HAVING count(*) > 1 \
             )",
            [],
            |row| row.get(0),
        )
        .map_err(display_error)?;
    if !duplicates {
        return Err("version-seven repair fixture contains no duplicate series numbers".into());
    }
    Ok(())
}

fn validate_repaired_series_numbers(path: &Path, options: &Options) -> Result<(), String> {
    let connection = Connection::open(path).map_err(display_error)?;
    let numbered: i64 = connection
        .query_row(
            "SELECT count(*) FROM series_memberships WHERE series_index IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .map_err(display_error)?;
    if numbered != i64::try_from(expected_series_count(options.books)).map_err(display_error)? {
        return Err(format!(
            "series repair retained {numbered} numbers instead of one per series"
        ));
    }
    let invalid: bool = connection
        .query_row(
            "SELECT EXISTS( \
                 SELECT 1 FROM series_memberships membership \
                 WHERE membership.series_index IS NOT NULL \
                   AND (EXISTS( \
                       SELECT 1 FROM series_memberships earlier \
                       WHERE earlier.series_id = membership.series_id \
                         AND earlier.book_id < membership.book_id \
                   ) OR (SELECT series_index FROM books WHERE id = membership.book_id) \
                       IS NOT membership.series_index) \
             )",
            [],
            |row| row.get(0),
        )
        .map_err(display_error)?;
    let stale_unnumbered_projection: bool = connection
        .query_row(
            "SELECT EXISTS( \
                 SELECT 1 FROM series_memberships membership \
                 JOIN books book ON book.id = membership.book_id \
                 WHERE membership.series_index IS NULL AND book.series_index IS NOT NULL \
             )",
            [],
            |row| row.get(0),
        )
        .map_err(display_error)?;
    if invalid || stale_unnumbered_projection {
        return Err(
            "series repair did not retain the lowest-ID number and exact projections".into(),
        );
    }
    Ok(())
}

fn validate_source_template(path: &Path, books: u64) -> Result<(), String> {
    let connection = Connection::open(path).map_err(display_error)?;
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(display_error)?;
    if version != 5 {
        return Err(format!("migration source schema is {version}, expected 5"));
    }
    expect_count(&connection, "books", books)?;
    expect_count(&connection, "book_assets", books)?;
    Ok(())
}

fn validate_migrated_candidate(path: &Path, options: &Options) -> Result<(), String> {
    let connection = Connection::open(path).map_err(display_error)?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(display_error)?;
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(display_error)?;
    if version != 9 {
        return Err(format!("candidate schema is {version}, expected 9"));
    }
    expect_count(&connection, "books", options.books)?;
    expect_count(&connection, "book_assets", options.books)?;
    expect_count(&connection, "book_contributors", options.books)?;
    expect_count(&connection, "contributors", options.books.min(20_000))?;
    expect_count(
        &connection,
        "series_entities",
        expected_series_count(options.books),
    )?;
    expect_count(&connection, "tags", 0)?;
    expect_count(&connection, "saved_searches", 0)?;
    let invalid_metadata_defaults: i64 = connection
        .query_row(
            "SELECT count(*) FROM books b LEFT JOIN book_metadata m ON m.book_id = b.id \
             WHERE m.publication_date IS NOT NULL OR coalesce(m.rating, 0) != 0",
            [],
            |row| row.get(0),
        )
        .map_err(display_error)?;
    if invalid_metadata_defaults != 0 {
        return Err(format!(
            "migration left {invalid_metadata_defaults} books with non-default canonical metadata"
        ));
    }
    expect_count(
        &connection,
        "book_covers",
        expected_cover_count(options.books, options.cover_every),
    )?;
    let unique_series_numbers: bool = connection
        .query_row(
            "SELECT EXISTS( \
                 SELECT 1 FROM sqlite_schema \
                 WHERE type = 'index' \
                   AND name = 'series_memberships_series_number_uidx' \
             )",
            [],
            |row| row.get(0),
        )
        .map_err(display_error)?;
    if !unique_series_numbers {
        return Err("migration did not install the unique series-number index".into());
    }

    let mismatches: i64 = connection
        .query_row(
            "SELECT count(*) FROM books b \
             JOIN book_assets a ON a.book_id = b.id \
             JOIN book_contributors bc ON bc.book_id = b.id \
             WHERE a.id <> b.id \
                OR b.authors <> bc.display_name_projection \
                OR b.authors_search <> b.authors \
                OR b.contributors_search <> b.authors \
                OR bc.role <> 'author' \
                OR bc.position <> 0",
            [],
            |row| row.get(0),
        )
        .map_err(display_error)?;
    if mismatches != 0 {
        return Err(format!(
            "migration produced {mismatches} projection or identity mismatches"
        ));
    }
    let foreign_keys = connection
        .prepare("PRAGMA foreign_key_check")
        .and_then(|mut statement| statement.exists([]))
        .map_err(display_error)?;
    if foreign_keys {
        return Err("migration produced a foreign-key violation".into());
    }
    connection
        .execute(
            "INSERT INTO books_fts(books_fts, rank) VALUES ('integrity-check', 1)",
            [],
        )
        .map_err(display_error)?;
    let fts_count: i64 = connection
        .query_row(
            "SELECT count(*) FROM books_fts WHERE books_fts MATCH '\"Contributor\"*'",
            [],
            |row| row.get(0),
        )
        .map_err(display_error)?;
    if fts_count != i64::try_from(options.books).map_err(display_error)? {
        return Err(format!("migrated FTS returned {fts_count} books"));
    }
    Ok(())
}

fn validate_failed_migration_rollback(options: &Options) -> Result<bool, String> {
    let candidate = candidate_path(&options.output, usize::MAX);
    copy_database(&options.database, &candidate)?;
    let connection = Connection::open(&candidate).map_err(display_error)?;
    connection
        .execute(
            "UPDATE books SET authors = 'invalid' || char(10) || 'author' WHERE id = 1",
            [],
        )
        .map_err(display_error)?;
    drop(connection);
    if LibraryDatabase::open(&candidate).is_ok() {
        remove_database_files(&candidate)?;
        return Err("injected invalid legacy author unexpectedly migrated".into());
    }
    let connection = Connection::open(&candidate).map_err(display_error)?;
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(display_error)?;
    let normalized_tables: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_schema \
             WHERE type = 'table' AND name = 'contributors'",
            [],
            |row| row.get(0),
        )
        .map_err(display_error)?;
    let old_fts_rows: i64 = connection
        .query_row(
            "SELECT count(*) FROM books_fts WHERE books_fts MATCH 'Title'",
            [],
            |row| row.get(0),
        )
        .map_err(display_error)?;
    drop(connection);
    remove_database_files(&candidate)?;
    Ok(version == 5
        && normalized_tables == 0
        && old_fts_rows == i64::try_from(options.books).map_err(display_error)?)
}

fn expect_count(connection: &Connection, table: &str, expected: u64) -> Result<(), String> {
    let count: i64 = connection
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .map_err(display_error)?;
    if count != i64::try_from(expected).map_err(display_error)? {
        return Err(format!("{table} count is {count}, expected {expected}"));
    }
    Ok(())
}

fn expected_series_count(books: u64) -> u64 {
    let memberships = (books / 10) * 7 + (books % 10).min(7);
    2_500.min(memberships)
}

fn expected_cover_count(books: u64, cover_every: u64) -> u64 {
    if books == 0 || cover_every == 0 {
        0
    } else {
        (books - 1) / cover_every + 1
    }
}

fn candidate_path(output: &Path, round: usize) -> PathBuf {
    output.with_file_name(format!(
        ".lectern-organisation-migration-{}-{round}.sqlite3",
        std::process::id()
    ))
}

fn version_seven_template_path(output: &Path) -> PathBuf {
    output.with_file_name(format!(
        ".lectern-organisation-migration-{}-v7.sqlite3",
        std::process::id()
    ))
}

fn copy_database(source: &Path, destination: &Path) -> Result<(), String> {
    if destination.exists() {
        return Err(format!(
            "migration candidate already exists: {}",
            destination.display()
        ));
    }
    fs::copy(source, destination).map_err(display_error)?;
    Ok(())
}

fn remove_database_files(path: &Path) -> Result<(), String> {
    for suffix in ["", "-wal", "-shm"] {
        let candidate = PathBuf::from(format!("{}{suffix}", path.display()));
        if candidate.exists() {
            fs::remove_file(&candidate).map_err(display_error)?;
        }
    }
    Ok(())
}

fn summarize_latency(samples: &[u64]) -> LatencySummary {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    LatencySummary {
        minimum: ns_to_ms(sorted[0]),
        p50: ns_to_ms(nearest_rank(&sorted, 50)),
        p95: ns_to_ms(nearest_rank(&sorted, 95)),
        p99: ns_to_ms(nearest_rank(&sorted, 99)),
        maximum: ns_to_ms(*sorted.last().expect("samples are non-empty")),
    }
}

fn nearest_rank(sorted: &[u64], percentile: usize) -> u64 {
    let rank = sorted.len().saturating_mul(percentile).div_ceil(100).max(1);
    sorted[rank - 1]
}

#[allow(clippy::cast_precision_loss)]
fn ns_to_ms(value: u64) -> f64 {
    value as f64 / 1_000_000.0
}

fn duration_ns(duration: Duration) -> Result<u64, String> {
    u64::try_from(duration.as_nanos()).map_err(display_error)
}

struct MemorySampler {
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    thread: thread::JoinHandle<Result<(), String>>,
}

impl MemorySampler {
    fn start() -> Result<Self, String> {
        let initial = resident_memory_bytes()?;
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicU64::new(initial));
        let sampler_stop = Arc::clone(&stop);
        let sampler_peak = Arc::clone(&peak);
        let thread = thread::Builder::new()
            .name("lectern-organisation-migration-rss".into())
            .spawn(move || {
                while !sampler_stop.load(Ordering::Acquire) {
                    let current = resident_memory_bytes()?;
                    sampler_peak.fetch_max(current, Ordering::Relaxed);
                    thread::sleep(Duration::from_millis(10));
                }
                sampler_peak.fetch_max(resident_memory_bytes()?, Ordering::Relaxed);
                Ok(())
            })
            .map_err(display_error)?;
        Ok(Self { stop, peak, thread })
    }

    fn finish(self) -> Result<u64, String> {
        self.stop.store(true, Ordering::Release);
        self.thread
            .join()
            .map_err(|_| "memory sampler panicked".to_owned())??;
        Ok(self.peak.load(Ordering::Relaxed))
    }
}

#[cfg(target_os = "linux")]
fn resident_memory_bytes() -> Result<u64, String> {
    let status = fs::read_to_string("/proc/self/status").map_err(display_error)?;
    let line = status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .ok_or_else(|| "/proc/self/status did not contain VmRSS".to_owned())?;
    let kib = line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "VmRSS did not contain a value".to_owned())?
        .parse::<u64>()
        .map_err(display_error)?;
    kib.checked_mul(1024)
        .ok_or_else(|| "VmRSS byte count overflowed".to_owned())
}

#[cfg(not(target_os = "linux"))]
fn resident_memory_bytes() -> Result<u64, String> {
    Err("migration RSS measurement currently requires Linux".into())
}

fn ensure_new_file(path: &Path) -> Result<(), String> {
    if path.exists() {
        return Err(format!("output already exists: {}", path.display()));
    }
    Ok(())
}

fn create_parent(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(display_error)?;
    }
    Ok(())
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(display_error)?;
    bytes.push(b'\n');
    fs::write(path, bytes).map_err(display_error)
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::{Options, expected_cover_count, expected_series_count, nearest_rank};

    #[test]
    fn parses_required_paths_and_numeric_options() {
        let (command, options) = Options::parse(
            [
                "migration",
                "--database",
                "source.sqlite3",
                "--output",
                "migration.json",
                "--iterations",
                "40",
                "--warmup",
                "10",
            ]
            .into_iter()
            .map(std::ffi::OsString::from),
        )
        .unwrap();
        assert_eq!(command, "migration");
        assert_eq!(options.iterations, 40);
        assert_eq!(options.warmup, 10);
    }

    #[test]
    fn deterministic_counts_cover_partial_cycles() {
        assert_eq!(expected_cover_count(0, 3), 0);
        assert_eq!(expected_cover_count(1, 3), 1);
        assert_eq!(expected_cover_count(4, 3), 2);
        assert_eq!(expected_series_count(10), 7);
        assert_eq!(expected_series_count(50_000), 2_500);
    }

    #[test]
    fn nearest_rank_returns_observed_samples() {
        let samples = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(nearest_rank(&samples, 50), 5);
        assert_eq!(nearest_rank(&samples, 95), 10);
    }
}
