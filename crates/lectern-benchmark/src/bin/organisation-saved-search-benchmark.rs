//! Release-mode saved-search manager and apply regression workload.

use std::{
    ffi::OsString,
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use lectern_core::{
    AssetHealth, BookFormat, LibraryQuery, SortOrder,
    organisation::{
        ContributorFacet, ContributorId, ExactFacets, SavedSearch, SeriesId, TagId, identity_key,
    },
};
use lectern_storage::LibraryDatabase;
use rusqlite::Connection;
use serde::Serialize;

const USAGE: &str = "Usage:
  organisation-saved-search-benchmark --database PATH --output PATH [OPTIONS]

Options:
  --books N       Logical books in the fixture (default: 50000)
  --iterations N  Measured iterations per scenario (default: 40)
  --warmup N      Warmup iterations per scenario (default: 10)
";
const PAGE_SIZE: u32 = 128;
const MANAGER_PAGE_SIZE: u32 = 100;
const SEEDED_SEARCHES: usize = 250;
const SEEDED_TAGS: u64 = 500;
const TAGS_PER_BOOK: u64 = 8;

#[derive(Clone, Debug)]
struct Options {
    database: PathBuf,
    output: PathBuf,
    books: u64,
    iterations: usize,
    warmup: usize,
}

impl Options {
    fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut arguments = arguments.into_iter();
        let mut options = Self {
            database: PathBuf::new(),
            output: PathBuf::new(),
            books: 50_000,
            iterations: 40,
            warmup: 10,
        };
        while let Some(argument) = arguments.next() {
            let name = argument
                .into_string()
                .map_err(|_| "option names must be UTF-8".to_owned())?;
            if matches!(name.as_str(), "help" | "--help" | "-h") {
                return Err(USAGE.to_owned());
            }
            let value = arguments
                .next()
                .ok_or_else(|| format!("missing value for {name}"))?;
            match name.as_str() {
                "--database" => options.database = PathBuf::from(value),
                "--output" => options.output = PathBuf::from(value),
                "--books" => options.books = parse_number(&name, value)?,
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
        if options.books == 0 || options.iterations == 0 {
            return Err("--books and --iterations must be greater than zero".into());
        }
        Ok(options)
    }
}

#[derive(Clone, Copy)]
enum Scenario {
    ManagerPage,
    ApplyProjection,
    ManagementCycle,
}

impl Scenario {
    const ALL: [Self; 3] = [Self::ManagerPage, Self::ApplyProjection, Self::ManagementCycle];

    const fn name(self) -> &'static str {
        match self {
            Self::ManagerPage => "bounded_saved_search_manager_page",
            Self::ApplyProjection => "saved_search_apply_first_page",
            Self::ManagementCycle => "saved_search_management_cycle",
        }
    }
}

#[derive(Serialize)]
struct BenchmarkResult {
    schema_version: u32,
    kind: &'static str,
    measured_at_unix_ms: u128,
    database_path: String,
    library_books: u64,
    saved_searches: usize,
    warmup_iterations: usize,
    measured_iterations: usize,
    manager_page_size: u32,
    query_page_size: u32,
    verified_checks: Vec<&'static str>,
    scenarios: Vec<ScenarioResult>,
}

#[derive(Serialize)]
struct ScenarioResult {
    name: &'static str,
    successful_operations: usize,
    observed_results: usize,
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

fn run(options: &Options) -> Result<(), String> {
    if !options.database.is_file() {
        return Err(format!(
            "saved-search database is not a file: {}",
            options.database.display()
        ));
    }
    ensure_new_file(&options.output)?;
    create_parent(&options.output)?;
    let before = library_fingerprint(&options.database)?;
    let mut database = LibraryDatabase::open(&options.database).map_err(display_error)?;
    if database.list_saved_searches().map_err(display_error)?.len() != SEEDED_SEARCHES {
        return Err("saved-search fixture does not contain 250 searches".into());
    }
    let rounds = options
        .warmup
        .checked_add(options.iterations)
        .ok_or_else(|| "saved-search iteration count overflowed".to_owned())?;
    let expected_apply_ids = expected_tag_ids(options.books, 2, PAGE_SIZE)?;
    let expected_apply_total = expected_tag_count(options.books, 2);
    let mut scenarios = Vec::with_capacity(Scenario::ALL.len());

    for scenario in Scenario::ALL {
        let mut samples = Vec::with_capacity(options.iterations);
        let mut observed_results = 0;
        for round in 0..rounds {
            let started = Instant::now();
            observed_results = match scenario {
                Scenario::ManagerPage => run_manager_page(&database)?,
                Scenario::ApplyProjection => run_apply_projection(
                    &mut database,
                    &expected_apply_ids,
                    expected_apply_total,
                )?,
                Scenario::ManagementCycle => run_management_cycle(&mut database)?,
            };
            let elapsed = started.elapsed();
            if round >= options.warmup {
                samples.push(duration_ns(elapsed)?);
            }
        }
        scenarios.push(ScenarioResult {
            name: scenario.name(),
            successful_operations: rounds,
            observed_results,
            latency_ms: summarize_latency(&samples),
            samples_ns: samples,
        });
    }
    drop(database);

    let after = library_fingerprint(&options.database)?;
    if before != after {
        return Err("saved-search management changed books, vocabulary, or assets".into());
    }
    let connection = Connection::open(&options.database).map_err(display_error)?;
    connection
        .execute(
            "INSERT INTO books_fts(books_fts, rank) VALUES ('integrity-check', 1)",
            [],
        )
        .map_err(display_error)?;
    drop(connection);

    let result = BenchmarkResult {
        schema_version: 1,
        kind: "organisation-saved-searches",
        measured_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(display_error)?
            .as_millis(),
        database_path: options.database.display().to_string(),
        library_books: options.books,
        saved_searches: SEEDED_SEARCHES,
        warmup_iterations: options.warmup,
        measured_iterations: options.iterations,
        manager_page_size: MANAGER_PAGE_SIZE,
        query_page_size: PAGE_SIZE,
        verified_checks: vec![
            "bounded_alphabetical_manager_page",
            "canonical_full_projection_round_trip",
            "exact_saved_projection_results",
            "explicit_update_and_stable_identity",
            "delete_preserves_books_vocabulary_assets",
            "fts_integrity",
        ],
        scenarios,
    };
    write_json(&options.output, &result)?;
    println!(
        "Measured {} saved-search scenarios over {} books and {} saved searches",
        Scenario::ALL.len(),
        options.books,
        SEEDED_SEARCHES
    );
    Ok(())
}

fn run_manager_page(database: &LibraryDatabase) -> Result<usize, String> {
    let searches = database
        .search_saved_searches("saved search 1", 0, MANAGER_PAGE_SIZE)
        .map_err(display_error)?;
    if searches.len() != usize::try_from(MANAGER_PAGE_SIZE).unwrap_or(100)
        || searches.first().map(|search| search.name.as_str()) != Some("Saved search 100")
        || searches.last().map(|search| search.name.as_str()) != Some("Saved search 199")
        || searches
            .windows(2)
            .any(|pair| identity_key(&pair[0].name) >= identity_key(&pair[1].name))
    {
        return Err("saved-search manager page was not bounded and alphabetical".into());
    }
    Ok(searches.len())
}

fn run_apply_projection(
    database: &mut LibraryDatabase,
    expected_ids: &[i64],
    expected_total: u64,
) -> Result<usize, String> {
    let searches = database.list_saved_searches().map_err(display_error)?;
    let saved = searches
        .iter()
        .find(|search| search.name == "Saved search 001")
        .ok_or_else(|| "saved projection 001 is absent".to_owned())?;
    if saved.query.search != "title:title tag:\"Tag 002\"" {
        return Err(format!(
            "saved projection 001 has unexpected canonical search {:?}",
            saved.query.search
        ));
    }
    let page = database
        .query_page(&saved.query, 0, PAGE_SIZE)
        .map_err(display_error)?;
    let ids = page
        .books
        .into_iter()
        .map(|book| book.id.value())
        .collect::<Vec<_>>();
    if page.total != expected_total || ids != expected_ids {
        return Err(format!(
            "saved projection returned total {} and {} IDs; expected {expected_total} and {} IDs",
            page.total,
            ids.len(),
            expected_ids.len()
        ));
    }
    Ok(ids.len())
}

fn run_management_cycle(database: &mut LibraryDatabase) -> Result<usize, String> {
    let name = "Benchmark saved lifecycle";
    let initial = LibraryQuery {
        search: "language:fr".into(),
        format: Some(BookFormat::Epub),
        asset_health: Some(AssetHealth::Available),
        facets: ExactFacets::new(
            vec![ContributorFacet {
                contributor: ContributorId::new(1),
                author_only: true,
            }],
            Some(SeriesId::new(1)),
            vec![TagId::new(1)],
            vec![TagId::new(9)],
        )
        .map_err(display_error)?,
        sort: SortOrder::Series,
    };
    let id = database
        .create_saved_search(name, &initial)
        .map_err(display_error)?;
    let created = only_saved_search(
        database
            .search_saved_searches("benchmark saved", 0, MANAGER_PAGE_SIZE)
            .map_err(display_error)?,
    )?;
    if created.id != id || created.name != name || created.query != initial {
        return Err("created saved search did not round-trip its complete projection".into());
    }

    let replacement = LibraryQuery {
        search: "publisher:publisher".into(),
        sort: SortOrder::RecentlyAdded,
        ..LibraryQuery::default()
    };
    database
        .update_saved_search(id, &replacement)
        .map_err(display_error)?;
    let renamed_name = "Benchmark renamed lifecycle";
    database
        .rename_saved_search(id, renamed_name)
        .map_err(display_error)?;
    let renamed = only_saved_search(
        database
            .search_saved_searches("benchmark renamed", 0, MANAGER_PAGE_SIZE)
            .map_err(display_error)?,
    )?;
    if renamed.id != id || renamed.name != renamed_name || renamed.query != replacement {
        return Err("updated saved search lost its stable identity or canonical projection".into());
    }
    if !database.delete_saved_search(id).map_err(display_error)?
        || !database
            .search_saved_searches("benchmark", 0, MANAGER_PAGE_SIZE)
            .map_err(display_error)?
            .is_empty()
    {
        return Err("saved-search delete did not remove only the temporary projection".into());
    }
    Ok(1)
}

fn only_saved_search(mut searches: Vec<SavedSearch>) -> Result<SavedSearch, String> {
    if searches.len() != 1 {
        return Err(format!(
            "expected one saved-search result, found {}",
            searches.len()
        ));
    }
    Ok(searches.pop().expect("one result is present"))
}

fn expected_tag_ids(books: u64, tag_id: u64, limit: u32) -> Result<Vec<i64>, String> {
    (0..books)
        .filter(|offset| book_has_tag(*offset, tag_id))
        .take(usize::try_from(limit).map_err(display_error)?)
        .map(|offset| i64::try_from(offset + 1).map_err(display_error))
        .collect()
}

fn expected_tag_count(books: u64, tag_id: u64) -> u64 {
    (0..books)
        .filter(|offset| book_has_tag(*offset, tag_id))
        .count()
        .try_into()
        .expect("fixture count fits u64")
}

fn book_has_tag(offset: u64, tag_id: u64) -> bool {
    (0..TAGS_PER_BOOK)
        .any(|tag_offset| 1 + (offset % SEEDED_TAGS + tag_offset) % SEEDED_TAGS == tag_id)
}

fn library_fingerprint(path: &Path) -> Result<Vec<i64>, String> {
    let connection = Connection::open(path).map_err(display_error)?;
    connection
        .query_row(
            "SELECT \
                 (SELECT count(*) FROM books), \
                 (SELECT coalesce(sum(id), 0) FROM books), \
                 (SELECT coalesce(sum(length(title) + length(authors) + \
                                      length(coalesce(series, ''))), 0) FROM books), \
                 (SELECT count(*) FROM book_assets), \
                 (SELECT coalesce(sum(id + length(path)), 0) FROM book_assets), \
                 (SELECT count(*) FROM contributors), \
                 (SELECT count(*) FROM series_entities), \
                 (SELECT count(*) FROM tags), \
                 (SELECT count(*) FROM book_contributors), \
                 (SELECT count(*) FROM series_memberships), \
                 (SELECT count(*) FROM book_tags), \
                 (SELECT count(*) FROM saved_searches)",
            [],
            |row| (0..12).map(|column| row.get(column)).collect(),
        )
        .map_err(display_error)
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

fn ensure_new_file(path: &Path) -> Result<(), String> {
    if path.exists() {
        return Err(format!("refusing to overwrite {}", path.display()));
    }
    Ok(())
}

fn create_parent(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(display_error)?;
    }
    Ok(())
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let file = File::create(path).map_err(display_error)?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, value).map_err(display_error)?;
    writer.write_all(b"\n").map_err(display_error)
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, path::PathBuf};

    use super::{Options, book_has_tag, nearest_rank};

    #[test]
    fn parses_required_paths_and_iterations() {
        let options = Options::parse(
            [
                "--database",
                "library.sqlite3",
                "--output",
                "result.json",
                "--books",
                "12",
                "--iterations",
                "4",
                "--warmup",
                "2",
            ]
            .into_iter()
            .map(OsString::from),
        )
        .unwrap();
        assert_eq!(options.database, PathBuf::from("library.sqlite3"));
        assert_eq!(options.output, PathBuf::from("result.json"));
        assert_eq!(options.books, 12);
        assert_eq!(options.iterations, 4);
        assert_eq!(options.warmup, 2);
    }

    #[test]
    fn seeded_tag_membership_wraps_deterministically() {
        assert!(book_has_tag(0, 2));
        assert!(book_has_tag(494, 2));
        assert!(!book_has_tag(2, 2));
    }

    #[test]
    fn nearest_rank_returns_observed_samples() {
        assert_eq!(nearest_rank(&[1, 2, 3, 4, 5], 95), 5);
    }
}
