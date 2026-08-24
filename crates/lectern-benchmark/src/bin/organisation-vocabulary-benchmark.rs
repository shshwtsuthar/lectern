//! Release-mode library-wide vocabulary mutation regression workload.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use lectern_core::{
    LibraryQuery,
    organisation::{ContributorId, ExactFacets, SeriesId, TagId},
};
use lectern_storage::LibraryDatabase;
use rusqlite::{Connection, TransactionBehavior, params};
use serde::Serialize;

const USAGE: &str = "Usage:
  organisation-vocabulary-benchmark --database PATH --output PATH [OPTIONS]

Options:
  --iterations N  Measured operations per scenario (default: 40)
  --warmup N      Warmup operations per scenario (default: 10)
";
const LIBRARY_BOOKS: u64 = 50_000;
const MATCHING_BOOKS: u64 = 10_000;
const SAVED_SEARCHES: u64 = 250;
const PAGE_SIZE: u32 = 128;
const MANAGER_PAGE_SIZE: u32 = 100;

#[derive(Debug)]
struct Options {
    database: PathBuf,
    output: PathBuf,
    iterations: usize,
    warmup: usize,
}

impl Options {
    fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut options = Self {
            database: PathBuf::new(),
            output: PathBuf::new(),
            iterations: 40,
            warmup: 10,
        };
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            let name = argument
                .into_string()
                .map_err(|_| "option names must be UTF-8".to_owned())?;
            if matches!(name.as_str(), "help" | "--help" | "-h") {
                return Err(USAGE.into());
            }
            let value = arguments
                .next()
                .ok_or_else(|| format!("missing value for {name}"))?;
            match name.as_str() {
                "--database" => options.database = value.into(),
                "--output" => options.output = value.into(),
                "--iterations" => options.iterations = parse_number(&name, value)?,
                "--warmup" => options.warmup = parse_number(&name, value)?,
                _ => return Err(format!("unknown option {name:?}")),
            }
        }
        if options.database.as_os_str().is_empty() || options.output.as_os_str().is_empty() {
            return Err("--database and --output are required".into());
        }
        if options.database == options.output {
            return Err("--database and --output must be distinct".into());
        }
        if options.iterations == 0 {
            return Err("--iterations must be greater than zero".into());
        }
        Ok(options)
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
    match Options::parse(std::env::args_os().skip(1)).and_then(|options| run(&options)) {
        Ok(()) => {}
        Err(error) if error == USAGE => println!("{USAGE}"),
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(2);
        }
    }
}

#[derive(Serialize)]
struct VocabularyResult {
    schema_version: u32,
    kind: &'static str,
    measured_at_unix_ms: u128,
    database_path: String,
    library_books: u64,
    matching_books: u64,
    saved_searches: u64,
    warmup_iterations: usize,
    measured_iterations: usize,
    page_size: u32,
    verified_checks: Vec<&'static str>,
    peak_rss_delta_bytes: u64,
    scenarios: Vec<VocabularyScenario>,
}

#[derive(Serialize)]
struct VocabularyScenario {
    name: &'static str,
    successful_operations: usize,
    books_affected_per_operation: u64,
    saved_searches_affected_per_operation: u64,
    refreshed_result_count: usize,
    latency_ms: LatencySummary,
    samples_ns: Vec<u64>,
}

#[derive(Serialize)]
struct LatencySummary {
    minimum: f64,
    p50: f64,
    p95: f64,
    p99: f64,
    maximum: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FixtureState {
    metadata_count: i64,
    metadata_bytes: i64,
    asset_count: i64,
    asset_bytes: i64,
    contributors: i64,
    credits: i64,
    series: i64,
    memberships: i64,
    tags: i64,
    book_tags: i64,
}

#[allow(clippy::too_many_lines)]
fn run(options: &Options) -> Result<(), String> {
    if !options.database.is_file() {
        return Err(format!(
            "vocabulary benchmark database is not a file: {}",
            options.database.display()
        ));
    }
    if options.output.exists() {
        return Err(format!(
            "vocabulary benchmark output already exists: {}",
            options.output.display()
        ));
    }
    if let Some(parent) = options.output.parent() {
        fs::create_dir_all(parent).map_err(display_error)?;
    }
    let baseline_state = fixture_state(&options.database)?;
    let mut database = LibraryDatabase::open(&options.database).map_err(display_error)?;
    if database.count().map_err(display_error)? != LIBRARY_BOOKS {
        return Err(format!(
            "vocabulary fixture must contain {LIBRARY_BOOKS} logical books"
        ));
    }
    validate_fixture(&options.database)?;
    verify_injected_rollback(&options.database, &mut database)?;
    let baseline_rss = peak_resident_bytes()?;
    let rounds = options
        .warmup
        .checked_add(options.iterations)
        .ok_or_else(|| "vocabulary iteration count overflowed".to_owned())?;

    let manager = measure_manager_reads(&database, rounds, options.warmup, options.iterations)?;
    let contributor = measure_contributor_merges(
        &options.database,
        &mut database,
        rounds,
        options.warmup,
        options.iterations,
    )?;
    prepare_series_backup(&options.database)?;
    let series = measure_series_merges(
        &options.database,
        &mut database,
        rounds,
        options.warmup,
        options.iterations,
    )?;
    let tag = measure_tag_merges(
        &options.database,
        &mut database,
        rounds,
        options.warmup,
        options.iterations,
    )?;
    drop(database);
    restore_final_projections(&options.database)?;
    if fixture_state(&options.database)? != baseline_state {
        return Err(
            "vocabulary workload changed ordinary metadata, assets, or base relations".into(),
        );
    }
    validate_fixture(&options.database)?;

    let result = VocabularyResult {
        schema_version: 1,
        kind: "organisation-vocabulary",
        measured_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(display_error)?
            .as_millis(),
        database_path: options.database.display().to_string(),
        library_books: LIBRARY_BOOKS,
        matching_books: MATCHING_BOOKS,
        saved_searches: SAVED_SEARCHES,
        warmup_iterations: options.warmup,
        measured_iterations: options.iterations,
        page_size: PAGE_SIZE,
        verified_checks: vec![
            "bounded_manager_page",
            "stable_target_identity",
            "source_identity_removed",
            "relationships_deduplicated",
            "positions_compacted",
            "saved_facets_rewritten",
            "projections_and_fts_rebuilt",
            "metadata_and_assets_unchanged",
            "rollback_verified",
        ],
        peak_rss_delta_bytes: peak_resident_bytes()?.saturating_sub(baseline_rss),
        scenarios: vec![manager, contributor, series, tag],
    };
    let bytes = serde_json::to_vec_pretty(&result).map_err(display_error)?;
    fs::write(&options.output, bytes).map_err(display_error)?;
    println!(
        "Measured {} bounded manager reads and contributor, series, and tag merges",
        options.iterations
    );
    Ok(())
}

fn measure_manager_reads(
    database: &LibraryDatabase,
    rounds: usize,
    warmup: usize,
    iterations: usize,
) -> Result<VocabularyScenario, String> {
    let mut samples = Vec::with_capacity(iterations);
    for round in 0..rounds {
        let started = Instant::now();
        let contributors = database
            .search_contributors("", 0, MANAGER_PAGE_SIZE)
            .map_err(display_error)?;
        let series = database
            .search_series("", 0, MANAGER_PAGE_SIZE)
            .map_err(display_error)?;
        let tags = database
            .search_tags("", 0, MANAGER_PAGE_SIZE)
            .map_err(display_error)?;
        let elapsed = started.elapsed();
        if contributors.len() != MANAGER_PAGE_SIZE as usize
            || series.len() != MANAGER_PAGE_SIZE as usize
            || tags.len() != MANAGER_PAGE_SIZE as usize
            || contributors[0].contributor.display_name != "Contributor 00001"
            || contributors[0].books != 8
            || series[0].series.name != "Series 0001"
            || series[0].books != 0
            || tags[0].tag.name != "Tag 001"
            || tags[0].books != 800
        {
            return Err("bounded manager page did not reconcile fixture data".into());
        }
        if round >= warmup {
            samples.push(duration_ns(elapsed)?);
        }
    }
    Ok(VocabularyScenario {
        name: "manager_search_page",
        successful_operations: rounds,
        books_affected_per_operation: 0,
        saved_searches_affected_per_operation: 0,
        refreshed_result_count: MANAGER_PAGE_SIZE as usize,
        latency_ms: summarize_latency(&samples),
        samples_ns: samples,
    })
}

fn measure_contributor_merges(
    path: &Path,
    database: &mut LibraryDatabase,
    rounds: usize,
    warmup: usize,
    iterations: usize,
) -> Result<VocabularyScenario, String> {
    let mut samples = Vec::with_capacity(iterations);
    let mut refreshed_result_count = 0;
    for round in 0..rounds {
        let fixture = setup_contributor_round(path, round)?;
        let started = Instant::now();
        let result = database
            .merge_contributors(fixture.source, fixture.target)
            .map_err(display_error)?;
        let refreshed = database
            .query_page(
                &LibraryQuery {
                    search: format!("contributor:\"{}\"", fixture.target_name),
                    ..LibraryQuery::default()
                },
                0,
                PAGE_SIZE,
            )
            .map_err(display_error)?;
        let elapsed = started.elapsed();
        if result.books != MATCHING_BOOKS
            || result.saved_searches != SAVED_SEARCHES
            || refreshed.total != MATCHING_BOOKS
            || refreshed.books.len() != PAGE_SIZE as usize
        {
            return Err(
                "contributor merge counts or refreshed projection did not reconcile".into(),
            );
        }
        refreshed_result_count = refreshed.books.len();
        verify_contributor_round(path, &fixture)?;
        if round >= warmup {
            samples.push(duration_ns(elapsed)?);
        }
        cleanup_contributor_round(path, &fixture)?;
    }
    Ok(VocabularyScenario {
        name: "contributor_merge_and_refresh",
        successful_operations: rounds,
        books_affected_per_operation: MATCHING_BOOKS,
        saved_searches_affected_per_operation: SAVED_SEARCHES,
        refreshed_result_count,
        latency_ms: summarize_latency(&samples),
        samples_ns: samples,
    })
}

fn measure_series_merges(
    path: &Path,
    database: &mut LibraryDatabase,
    rounds: usize,
    warmup: usize,
    iterations: usize,
) -> Result<VocabularyScenario, String> {
    let mut samples = Vec::with_capacity(iterations);
    let mut refreshed_result_count = 0;
    for round in 0..rounds {
        let fixture = setup_series_round(path, round)?;
        let started = Instant::now();
        let result = database
            .merge_series(fixture.source, fixture.target)
            .map_err(display_error)?;
        let refreshed = database
            .query_page(
                &LibraryQuery {
                    facets: ExactFacets::new(
                        Vec::new(),
                        Some(fixture.target),
                        Vec::new(),
                        Vec::new(),
                    )
                    .map_err(display_error)?,
                    ..LibraryQuery::default()
                },
                0,
                PAGE_SIZE,
            )
            .map_err(display_error)?;
        let elapsed = started.elapsed();
        if result.books != MATCHING_BOOKS
            || result.saved_searches != SAVED_SEARCHES
            || refreshed.total != MATCHING_BOOKS
            || refreshed.books.len() != PAGE_SIZE as usize
        {
            return Err("series merge counts or refreshed projection did not reconcile".into());
        }
        refreshed_result_count = refreshed.books.len();
        verify_series_round(path, &fixture)?;
        if round >= warmup {
            samples.push(duration_ns(elapsed)?);
        }
        cleanup_series_round(path, &fixture)?;
    }
    Ok(VocabularyScenario {
        name: "series_merge_and_refresh",
        successful_operations: rounds,
        books_affected_per_operation: MATCHING_BOOKS,
        saved_searches_affected_per_operation: SAVED_SEARCHES,
        refreshed_result_count,
        latency_ms: summarize_latency(&samples),
        samples_ns: samples,
    })
}

fn measure_tag_merges(
    path: &Path,
    database: &mut LibraryDatabase,
    rounds: usize,
    warmup: usize,
    iterations: usize,
) -> Result<VocabularyScenario, String> {
    let mut samples = Vec::with_capacity(iterations);
    let mut refreshed_result_count = 0;
    for round in 0..rounds {
        let fixture = setup_tag_round(path, round)?;
        let started = Instant::now();
        let result = database
            .merge_tags(fixture.source, fixture.target)
            .map_err(display_error)?;
        let refreshed = database
            .query_page(
                &LibraryQuery {
                    facets: ExactFacets::new(Vec::new(), None, vec![fixture.target], Vec::new())
                        .map_err(display_error)?,
                    ..LibraryQuery::default()
                },
                0,
                PAGE_SIZE,
            )
            .map_err(display_error)?;
        let elapsed = started.elapsed();
        if result.books != MATCHING_BOOKS
            || result.saved_searches != SAVED_SEARCHES
            || refreshed.total != MATCHING_BOOKS
            || refreshed.books.len() != PAGE_SIZE as usize
        {
            return Err("tag merge counts or refreshed projection did not reconcile".into());
        }
        refreshed_result_count = refreshed.books.len();
        verify_tag_round(path, &fixture)?;
        if round >= warmup {
            samples.push(duration_ns(elapsed)?);
        }
        cleanup_tag_round(path, &fixture)?;
    }
    Ok(VocabularyScenario {
        name: "tag_merge_and_refresh",
        successful_operations: rounds,
        books_affected_per_operation: MATCHING_BOOKS,
        saved_searches_affected_per_operation: SAVED_SEARCHES,
        refreshed_result_count,
        latency_ms: summarize_latency(&samples),
        samples_ns: samples,
    })
}

struct ContributorFixture {
    source: ContributorId,
    target: ContributorId,
    target_name: String,
}

fn setup_contributor_round(path: &Path, round: usize) -> Result<ContributorFixture, String> {
    let mut connection = benchmark_connection(path)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(display_error)?;
    let source_name = format!("Merge source contributor {round:03}");
    let target_name = format!("Merge target contributor {round:03}");
    let source = insert_contributor(&transaction, &source_name)?;
    let target = insert_contributor(&transaction, &target_name)?;
    transaction
        .execute(
            "INSERT INTO book_contributors( \
                 book_id, contributor_id, role, position, \
                 display_name_projection, sort_key_projection \
             ) SELECT id, ?1, 'other', 0, ?2, ?3 FROM books WHERE language = 'fr'",
            params![
                source.value(),
                source_name,
                source_name.to_ascii_lowercase()
            ],
        )
        .map_err(display_error)?;
    transaction
        .execute(
            "INSERT INTO book_contributors( \
                 book_id, contributor_id, role, position, \
                 display_name_projection, sort_key_projection \
             ) SELECT id, ?1, 'other', 1, ?2, ?3 FROM books \
               WHERE language = 'fr' AND id % 10 = 1",
            params![
                target.value(),
                target_name,
                target_name.to_ascii_lowercase()
            ],
        )
        .map_err(display_error)?;
    transaction
        .execute(
            "INSERT INTO saved_search_contributors(saved_search_id, contributor_id, author_only) \
             SELECT id, ?1, 0 FROM saved_searches",
            [source.value()],
        )
        .map_err(display_error)?;
    transaction
        .execute(
            "INSERT INTO saved_search_contributors(saved_search_id, contributor_id, author_only) \
             SELECT id, ?1, 1 FROM saved_searches WHERE id <= 125",
            [target.value()],
        )
        .map_err(display_error)?;
    transaction.commit().map_err(display_error)?;
    Ok(ContributorFixture {
        source,
        target,
        target_name,
    })
}

fn insert_contributor(
    transaction: &rusqlite::Transaction<'_>,
    name: &str,
) -> Result<ContributorId, String> {
    transaction
        .execute(
            "INSERT INTO contributors(display_name, sort_name, identity_key, sort_key) \
             VALUES (?1, ?1, ?2, ?2)",
            params![name, name.to_ascii_lowercase()],
        )
        .map_err(display_error)?;
    Ok(ContributorId::new(transaction.last_insert_rowid()))
}

fn verify_contributor_round(path: &Path, fixture: &ContributorFixture) -> Result<(), String> {
    let connection = benchmark_connection(path)?;
    let observed: (i64, i64, i64, i64, i64) = connection
        .query_row(
            "SELECT \
                 (SELECT count(*) FROM contributors WHERE id = ?1), \
                 (SELECT count(*) FROM contributors WHERE id = ?2), \
                 (SELECT count(DISTINCT book_id) FROM book_contributors \
                  WHERE contributor_id = ?2), \
                 (SELECT count(*) FROM book_contributors \
                  WHERE contributor_id = ?2 AND role = 'other' AND position = 0), \
                 (SELECT count(*) FROM saved_search_contributors \
                  WHERE contributor_id = ?2)",
            params![fixture.source.value(), fixture.target.value()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .map_err(display_error)?;
    if observed != (0, 1, 10_000, 10_000, 250) {
        return Err(format!(
            "contributor merge verification failed: {observed:?}"
        ));
    }
    let author_only: i64 = connection
        .query_row(
            "SELECT count(*) FROM saved_search_contributors \
             WHERE contributor_id = ?1 AND author_only = 1",
            [fixture.target.value()],
            |row| row.get(0),
        )
        .map_err(display_error)?;
    if author_only != 125 {
        return Err("contributor saved facets did not preserve the stricter role".into());
    }
    Ok(())
}

fn cleanup_contributor_round(path: &Path, fixture: &ContributorFixture) -> Result<(), String> {
    let mut connection = benchmark_connection(path)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(display_error)?;
    transaction
        .execute(
            "DELETE FROM saved_search_contributors WHERE contributor_id IN (?1, ?2)",
            params![fixture.source.value(), fixture.target.value()],
        )
        .map_err(display_error)?;
    transaction
        .execute(
            "DELETE FROM book_contributors WHERE contributor_id IN (?1, ?2)",
            params![fixture.source.value(), fixture.target.value()],
        )
        .map_err(display_error)?;
    transaction
        .execute(
            "DELETE FROM contributors WHERE id IN (?1, ?2)",
            params![fixture.source.value(), fixture.target.value()],
        )
        .map_err(display_error)?;
    transaction.commit().map_err(display_error)
}

struct SeriesFixture {
    source: SeriesId,
    target: SeriesId,
}

fn prepare_series_backup(path: &Path) -> Result<(), String> {
    let connection = benchmark_connection(path)?;
    connection
        .execute_batch(
            "DROP TABLE IF EXISTS benchmark_series_backup; \
             CREATE TABLE benchmark_series_backup AS \
             SELECT sm.* FROM series_memberships sm \
             JOIN books b ON b.id = sm.book_id WHERE b.language = 'fr';",
        )
        .map_err(display_error)
}

fn setup_series_round(path: &Path, round: usize) -> Result<SeriesFixture, String> {
    let mut connection = benchmark_connection(path)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(display_error)?;
    let source = insert_series(&transaction, &format!("Merge source series {round:03}"))?;
    let target = insert_series(&transaction, &format!("Merge target series {round:03}"))?;
    transaction
        .execute(
            "DELETE FROM series_memberships WHERE book_id IN ( \
                 SELECT id FROM books WHERE language = 'fr' \
             )",
            [],
        )
        .map_err(display_error)?;
    transaction
        .execute(
            "INSERT INTO series_memberships( \
                 book_id, series_id, series_index, name_projection, key_projection \
             ) SELECT id, ?1, (id % 100) * 1000000, ?2, ?3 \
               FROM books WHERE language = 'fr'",
            params![
                source.value(),
                format!("Merge source series {round:03}"),
                format!("merge source series {round:03}")
            ],
        )
        .map_err(display_error)?;
    transaction
        .execute("UPDATE saved_searches SET series_id = ?1", [source.value()])
        .map_err(display_error)?;
    transaction.commit().map_err(display_error)?;
    Ok(SeriesFixture { source, target })
}

fn insert_series(transaction: &rusqlite::Transaction<'_>, name: &str) -> Result<SeriesId, String> {
    transaction
        .execute(
            "INSERT INTO series_entities(name, identity_key) VALUES (?1, ?2)",
            params![name, name.to_ascii_lowercase()],
        )
        .map_err(display_error)?;
    Ok(SeriesId::new(transaction.last_insert_rowid()))
}

fn verify_series_round(path: &Path, fixture: &SeriesFixture) -> Result<(), String> {
    let connection = benchmark_connection(path)?;
    let observed: (i64, i64, i64, i64) = connection
        .query_row(
            "SELECT \
                 (SELECT count(*) FROM series_entities WHERE id = ?1), \
                 (SELECT count(*) FROM series_entities WHERE id = ?2), \
                 (SELECT count(*) FROM series_memberships WHERE series_id = ?2), \
                 (SELECT count(*) FROM saved_searches WHERE series_id = ?2)",
            params![fixture.source.value(), fixture.target.value()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(display_error)?;
    if observed != (0, 1, 10_000, 250) {
        return Err(format!("series merge verification failed: {observed:?}"));
    }
    Ok(())
}

fn cleanup_series_round(path: &Path, fixture: &SeriesFixture) -> Result<(), String> {
    let mut connection = benchmark_connection(path)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(display_error)?;
    transaction
        .execute("UPDATE saved_searches SET series_id = NULL", [])
        .map_err(display_error)?;
    transaction
        .execute(
            "DELETE FROM series_memberships WHERE series_id IN (?1, ?2)",
            params![fixture.source.value(), fixture.target.value()],
        )
        .map_err(display_error)?;
    transaction
        .execute(
            "INSERT INTO series_memberships \
             SELECT * FROM benchmark_series_backup",
            [],
        )
        .map_err(display_error)?;
    transaction
        .execute(
            "DELETE FROM series_entities WHERE id IN (?1, ?2)",
            params![fixture.source.value(), fixture.target.value()],
        )
        .map_err(display_error)?;
    transaction.commit().map_err(display_error)
}

struct TagFixture {
    source: TagId,
    target: TagId,
}

fn setup_tag_round(path: &Path, round: usize) -> Result<TagFixture, String> {
    let mut connection = benchmark_connection(path)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(display_error)?;
    let source = insert_tag(&transaction, &format!("Merge source tag {round:03}"))?;
    let target = insert_tag(&transaction, &format!("Merge target tag {round:03}"))?;
    transaction
        .execute(
            "INSERT INTO book_tags(book_id, tag_id) \
             SELECT id, ?1 FROM books WHERE language = 'fr'",
            [source.value()],
        )
        .map_err(display_error)?;
    transaction
        .execute(
            "INSERT INTO book_tags(book_id, tag_id) \
             SELECT id, ?1 FROM books WHERE language = 'fr' AND id % 10 = 1",
            [target.value()],
        )
        .map_err(display_error)?;
    transaction
        .execute(
            "INSERT INTO saved_search_included_tags(saved_search_id, tag_id) \
             SELECT id, ?1 FROM saved_searches",
            [source.value()],
        )
        .map_err(display_error)?;
    transaction
        .execute(
            "INSERT INTO saved_search_excluded_tags(saved_search_id, tag_id) \
             SELECT id, ?1 FROM saved_searches WHERE id <= 125",
            [target.value()],
        )
        .map_err(display_error)?;
    transaction.commit().map_err(display_error)?;
    Ok(TagFixture { source, target })
}

fn insert_tag(transaction: &rusqlite::Transaction<'_>, name: &str) -> Result<TagId, String> {
    transaction
        .execute(
            "INSERT INTO tags(name, identity_key) VALUES (?1, ?2)",
            params![name, name.to_ascii_lowercase()],
        )
        .map_err(display_error)?;
    Ok(TagId::new(transaction.last_insert_rowid()))
}

fn verify_tag_round(path: &Path, fixture: &TagFixture) -> Result<(), String> {
    let connection = benchmark_connection(path)?;
    let observed: (i64, i64, i64, i64, i64) = connection
        .query_row(
            "SELECT \
                 (SELECT count(*) FROM tags WHERE id = ?1), \
                 (SELECT count(*) FROM tags WHERE id = ?2), \
                 (SELECT count(*) FROM book_tags WHERE tag_id = ?2), \
                 (SELECT count(*) FROM saved_search_included_tags WHERE tag_id = ?2), \
                 (SELECT count(*) FROM saved_search_excluded_tags WHERE tag_id = ?2)",
            params![fixture.source.value(), fixture.target.value()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .map_err(display_error)?;
    if observed != (0, 1, 10_000, 125, 125) {
        return Err(format!("tag merge verification failed: {observed:?}"));
    }
    Ok(())
}

fn cleanup_tag_round(path: &Path, fixture: &TagFixture) -> Result<(), String> {
    let mut connection = benchmark_connection(path)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(display_error)?;
    for table in ["saved_search_included_tags", "saved_search_excluded_tags"] {
        transaction
            .execute(
                &format!("DELETE FROM {table} WHERE tag_id IN (?1, ?2)"),
                params![fixture.source.value(), fixture.target.value()],
            )
            .map_err(display_error)?;
    }
    transaction
        .execute(
            "DELETE FROM book_tags WHERE tag_id IN (?1, ?2)",
            params![fixture.source.value(), fixture.target.value()],
        )
        .map_err(display_error)?;
    transaction
        .execute(
            "DELETE FROM tags WHERE id IN (?1, ?2)",
            params![fixture.source.value(), fixture.target.value()],
        )
        .map_err(display_error)?;
    transaction.commit().map_err(display_error)
}

fn verify_injected_rollback(path: &Path, database: &mut LibraryDatabase) -> Result<(), String> {
    let fixture = setup_contributor_round(path, 9_999)?;
    let connection = benchmark_connection(path)?;
    let failing_book: i64 = connection
        .query_row(
            "SELECT id FROM books WHERE language = 'fr' ORDER BY id LIMIT 1",
            [],
            |row| row.get(0),
        )
        .map_err(display_error)?;
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER benchmark_injected_merge_failure \
             BEFORE UPDATE OF contributors_search ON books WHEN old.id = {failing_book} BEGIN \
                 SELECT raise(ABORT, 'injected vocabulary merge failure'); \
             END;"
        ))
        .map_err(display_error)?;
    if database
        .merge_contributors(fixture.source, fixture.target)
        .is_ok()
    {
        return Err("injected contributor merge unexpectedly committed".into());
    }
    let retained: (i64, i64, i64) = connection
        .query_row(
            "SELECT \
                 (SELECT count(DISTINCT book_id) FROM book_contributors \
                  WHERE contributor_id = ?1), \
                 (SELECT count(*) FROM saved_search_contributors WHERE contributor_id = ?1), \
                 (SELECT count(*) FROM sqlite_schema \
                  WHERE type = 'trigger' AND name = 'books_after_update')",
            [fixture.source.value()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(display_error)?;
    connection
        .execute_batch("DROP TRIGGER benchmark_injected_merge_failure")
        .map_err(display_error)?;
    if retained != (10_000, 250, 1) {
        return Err(
            "injected contributor merge did not roll back relations and the FTS trigger".into(),
        );
    }
    cleanup_contributor_round(path, &fixture)
}

fn restore_final_projections(path: &Path) -> Result<(), String> {
    let connection = benchmark_connection(path)?;
    connection
        .execute_batch(
            "UPDATE books AS b SET contributors_search = coalesce(( \
                 SELECT group_concat(display_name_projection, ' ') FROM ( \
                     SELECT display_name_projection FROM book_contributors WHERE book_id = b.id \
                     ORDER BY CASE role WHEN 'author' THEN 0 WHEN 'editor' THEN 1 \
                         WHEN 'translator' THEN 2 WHEN 'illustrator' THEN 3 ELSE 4 END, \
                         position, contributor_id \
                 ) \
             ), '') WHERE language = 'fr'; \
             UPDATE books AS b SET \
                 series = (SELECT name_projection FROM series_memberships WHERE book_id = b.id), \
                 series_key = (SELECT key_projection FROM series_memberships WHERE book_id = b.id), \
                 series_index = (SELECT series_index FROM series_memberships WHERE book_id = b.id) \
             WHERE language = 'fr'; \
             UPDATE books AS b SET tags_search = coalesce(( \
                 SELECT group_concat(name, ' ') FROM ( \
                     SELECT t.name AS name FROM book_tags bt JOIN tags t ON t.id = bt.tag_id \
                     WHERE bt.book_id = b.id ORDER BY t.identity_key, t.id \
                 ) \
             ), '') WHERE language = 'fr'; \
             DROP TABLE benchmark_series_backup;",
        )
        .map_err(display_error)
}

fn validate_fixture(path: &Path) -> Result<(), String> {
    let connection = benchmark_connection(path)?;
    let observed: (i64, i64, i64, i64) = connection
        .query_row(
            "SELECT \
                 (SELECT count(*) FROM books), \
                 (SELECT count(*) FROM books WHERE language = 'fr'), \
                 (SELECT count(*) FROM saved_searches), \
                 (SELECT count(*) FROM saved_search_contributors) + \
                 (SELECT count(*) FROM saved_search_included_tags) + \
                 (SELECT count(*) FROM saved_search_excluded_tags) + \
                 (SELECT count(*) FROM saved_searches WHERE series_id IS NOT NULL)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(display_error)?;
    if observed != (50_000, 10_000, 250, 0) {
        return Err(format!(
            "vocabulary fixture did not reconcile: {observed:?}"
        ));
    }
    Ok(())
}

fn fixture_state(path: &Path) -> Result<FixtureState, String> {
    let connection = benchmark_connection(path)?;
    connection
        .query_row(
            "SELECT \
                 (SELECT count(*) FROM books), \
                 (SELECT coalesce(sum(length(title) + length(authors) + \
                     length(coalesce(series, '')) + length(coalesce(publisher, '')) + \
                     length(coalesce(language, '')) + length(coalesce(description, ''))), 0) \
                  FROM books), \
                 (SELECT count(*) FROM book_assets), \
                 (SELECT coalesce(sum(length(path)), 0) FROM book_assets), \
                 (SELECT count(*) FROM contributors), \
                 (SELECT count(*) FROM book_contributors), \
                 (SELECT count(*) FROM series_entities), \
                 (SELECT count(*) FROM series_memberships), \
                 (SELECT count(*) FROM tags), \
                 (SELECT count(*) FROM book_tags)",
            [],
            |row| {
                Ok(FixtureState {
                    metadata_count: row.get(0)?,
                    metadata_bytes: row.get(1)?,
                    asset_count: row.get(2)?,
                    asset_bytes: row.get(3)?,
                    contributors: row.get(4)?,
                    credits: row.get(5)?,
                    series: row.get(6)?,
                    memberships: row.get(7)?,
                    tags: row.get(8)?,
                    book_tags: row.get(9)?,
                })
            },
        )
        .map_err(display_error)
}

fn benchmark_connection(path: &Path) -> Result<Connection, String> {
    let connection = Connection::open(path).map_err(display_error)?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(display_error)?;
    Ok(connection)
}

fn peak_resident_bytes() -> Result<u64, String> {
    let status = fs::read_to_string("/proc/self/status").map_err(display_error)?;
    let kibibytes = status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.split_whitespace().next())
        .ok_or_else(|| "Linux process status did not report VmHWM".to_owned())?
        .parse::<u64>()
        .map_err(display_error)?;
    kibibytes
        .checked_mul(1024)
        .ok_or_else(|| "peak resident byte count overflowed".to_owned())
}

fn duration_ns(duration: Duration) -> Result<u64, String> {
    u64::try_from(duration.as_nanos()).map_err(display_error)
}

fn summarize_latency(samples: &[u64]) -> LatencySummary {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    LatencySummary {
        minimum: nanos_to_ms(*sorted.first().expect("measured samples are non-empty")),
        p50: nanos_to_ms(nearest_rank(&sorted, 50)),
        p95: nanos_to_ms(nearest_rank(&sorted, 95)),
        p99: nanos_to_ms(nearest_rank(&sorted, 99)),
        maximum: nanos_to_ms(*sorted.last().expect("measured samples are non-empty")),
    }
}

fn nearest_rank(sorted: &[u64], percentile: usize) -> u64 {
    let rank = (percentile * sorted.len()).div_ceil(100);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn nanos_to_ms(value: u64) -> f64 {
    Duration::from_nanos(value).as_secs_f64() * 1_000.0
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::{Options, nearest_rank};

    #[test]
    fn parses_required_paths_and_iterations() {
        let options = Options::parse([
            "--database".into(),
            "library.sqlite3".into(),
            "--output".into(),
            "vocabulary.json".into(),
            "--iterations".into(),
            "7".into(),
        ])
        .unwrap();
        assert_eq!(options.iterations, 7);
    }

    #[test]
    fn nearest_rank_returns_observed_samples() {
        assert_eq!(nearest_rank(&[1, 2, 3, 4, 5], 95), 5);
    }
}
