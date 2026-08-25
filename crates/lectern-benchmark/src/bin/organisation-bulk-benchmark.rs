//! Release-mode query-backed selection and atomic bulk-tag regression workload.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use lectern_core::{
    LibraryQuery,
    organisation::{BookSelection, BulkTagEdit, ExactFacets, TagId, TagReference},
};
use lectern_storage::LibraryDatabase;
use rusqlite::Connection;
use serde::Serialize;

const USAGE: &str = "Usage:
  organisation-bulk-benchmark --database PATH --output PATH [OPTIONS]

Options:
  --books N       Logical books in the fixture (default: 50000)
  --iterations N  Measured forward/inverse operations (default: 40)
  --warmup N      Warmup forward/inverse operations (default: 10)
";
const MATCHING_BOOKS: u64 = 10_000;
const PAGE_SIZE: u32 = 128;
type LibraryState = ((i64, i64), (i64, i64));

#[derive(Debug)]
struct Options {
    database: PathBuf,
    output: PathBuf,
    books: u64,
    iterations: usize,
    warmup: usize,
}

impl Options {
    fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut options = Self {
            database: PathBuf::new(),
            output: PathBuf::new(),
            books: 50_000,
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
struct BulkResult {
    schema_version: u32,
    kind: &'static str,
    measured_at_unix_ms: u128,
    database_path: String,
    library_books: u64,
    matching_books: u64,
    warmup_iterations: usize,
    measured_iterations: usize,
    page_size: u32,
    verified_checks: Vec<&'static str>,
    selection_materialized_summaries: u64,
    peak_rss_delta_bytes: u64,
    scenarios: Vec<BulkScenario>,
}

#[derive(Serialize)]
struct BulkScenario {
    name: &'static str,
    successful_operations: usize,
    books_matched_per_operation: u64,
    relationships_added_per_operation: u64,
    relationships_removed_per_operation: u64,
    tags_created_per_operation: u64,
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

#[allow(clippy::too_many_lines)]
fn run(options: &Options) -> Result<(), String> {
    if !options.database.is_file() {
        return Err(format!(
            "bulk benchmark database is not a file: {}",
            options.database.display()
        ));
    }
    if options.output.exists() {
        return Err(format!(
            "bulk benchmark output already exists: {}",
            options.output.display()
        ));
    }
    if let Some(parent) = options.output.parent() {
        fs::create_dir_all(parent).map_err(display_error)?;
    }
    validate_library(&options.database, options.books)?;
    let baseline_state = library_state(&options.database)?;
    let target_query = LibraryQuery {
        search: "language:fr".into(),
        ..LibraryQuery::default()
    };
    let mut database = LibraryDatabase::open(&options.database).map_err(display_error)?;
    let initial = database
        .selection_snapshot(&target_query)
        .map_err(display_error)?;
    if initial.matching_books != MATCHING_BOOKS {
        return Err(format!(
            "bulk fixture matched {} books, expected {MATCHING_BOOKS}",
            initial.matching_books
        ));
    }

    let initial_selection =
        BookSelection::all_matching(target_query.clone(), initial.generation, Vec::new());
    let baseline_tag = database
        .apply_bulk_tags(
            &initial_selection,
            &BulkTagEdit {
                add: vec![TagReference::New("Bulk baseline".into())],
                remove: Vec::new(),
            },
        )
        .map_err(display_error)?;
    if baseline_tag.books_matched != MATCHING_BOOKS
        || baseline_tag.relationships_added != MATCHING_BOOKS
    {
        return Err("could not establish the deterministic removable baseline tag".into());
    }
    let baseline_tag_id = exact_tag_id(&database, "Bulk baseline")?;
    verify_injected_rollback(&mut database, &target_query, baseline_tag_id)?;

    let baseline_hwm = peak_resident_bytes()?;
    let rounds = options
        .warmup
        .checked_add(options.iterations)
        .ok_or_else(|| "bulk iteration count overflowed".to_owned())?;
    let mut samples = Vec::with_capacity(options.iterations);
    let mut refreshed_result_count = 0;

    for round in 0..rounds {
        let add_name_a = format!("Bulk added A {round:03}");
        let add_name_b = format!("Bulk added B {round:03}");
        let snapshot = database
            .selection_snapshot(&target_query)
            .map_err(display_error)?;
        let selection =
            BookSelection::all_matching(target_query.clone(), snapshot.generation, Vec::new());
        let started = Instant::now();
        let forward = database
            .apply_bulk_tags(
                &selection,
                &BulkTagEdit {
                    add: vec![
                        TagReference::New(add_name_a.clone()),
                        TagReference::New(add_name_b.clone()),
                    ],
                    remove: vec![baseline_tag_id],
                },
            )
            .map_err(display_error)?;
        validate_forward(&forward)?;
        let refreshed = database
            .query_page(
                &LibraryQuery {
                    search: format!("tag:\"{add_name_a}\" tag:\"{add_name_b}\""),
                    ..LibraryQuery::default()
                },
                0,
                PAGE_SIZE,
            )
            .map_err(display_error)?;
        let elapsed = started.elapsed();
        if refreshed.total != MATCHING_BOOKS || refreshed.books.len() != PAGE_SIZE as usize {
            return Err("bulk refresh did not expose the exact changed projection".into());
        }
        refreshed_result_count = refreshed.books.len();
        if round >= options.warmup {
            samples.push(duration_ns(elapsed)?);
        }

        let added_a = exact_tag_id(&database, &add_name_a)?;
        let added_b = exact_tag_id(&database, &add_name_b)?;
        let inverse_snapshot = database
            .selection_snapshot(&target_query)
            .map_err(display_error)?;
        let inverse_selection = BookSelection::all_matching(
            target_query.clone(),
            inverse_snapshot.generation,
            Vec::new(),
        );
        let inverse = database
            .apply_bulk_tags(
                &inverse_selection,
                &BulkTagEdit {
                    add: vec![TagReference::Existing(baseline_tag_id)],
                    remove: vec![added_a, added_b],
                },
            )
            .map_err(display_error)?;
        if inverse.books_matched != MATCHING_BOOKS
            || inverse.relationships_added != MATCHING_BOOKS
            || inverse.relationships_removed != MATCHING_BOOKS * 2
            || inverse.tags_created != 0
        {
            return Err("inverse bulk operation did not restore exact relationships".into());
        }
        let absent = database
            .query_page(
                &LibraryQuery {
                    search: format!("tag:\"{add_name_a}\""),
                    ..LibraryQuery::default()
                },
                0,
                1,
            )
            .map_err(display_error)?;
        if absent.total != 0 {
            return Err("inverse bulk operation left stale FTS/filter visibility".into());
        }
    }

    drop(database);
    let final_state = library_state(&options.database)?;
    if baseline_state != final_state {
        return Err("bulk workload changed ordinary metadata or assets".into());
    }
    let result = BulkResult {
        schema_version: 1,
        kind: "organisation-bulk-tags",
        measured_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(display_error)?
            .as_millis(),
        database_path: options.database.display().to_string(),
        library_books: options.books,
        matching_books: MATCHING_BOOKS,
        warmup_iterations: options.warmup,
        measured_iterations: options.iterations,
        page_size: PAGE_SIZE,
        verified_checks: vec![
            "exact_matched_added_removed_created_counts",
            "ordinary_metadata_unchanged",
            "assets_unchanged",
            "exact_relationship_sets",
            "fts_and_filter_visibility",
            "injected_failure_rolls_back",
            "inverse_operation_restores_relationships",
            "selection_does_not_materialize_book_summaries",
        ],
        selection_materialized_summaries: 0,
        peak_rss_delta_bytes: peak_resident_bytes()?.saturating_sub(baseline_hwm),
        scenarios: vec![BulkScenario {
            name: "bulk_tag_apply_and_refresh",
            successful_operations: rounds,
            books_matched_per_operation: MATCHING_BOOKS,
            relationships_added_per_operation: MATCHING_BOOKS * 2,
            relationships_removed_per_operation: MATCHING_BOOKS,
            tags_created_per_operation: 2,
            refreshed_result_count,
            latency_ms: summarize_latency(&samples),
            samples_ns: samples,
        }],
    };
    write_json(&options.output, &result)?;
    println!(
        "Measured {} atomic 10000-book bulk tag operations",
        options.iterations
    );
    Ok(())
}

fn validate_forward(result: &lectern_core::organisation::BulkTagResult) -> Result<(), String> {
    if result.books_matched != MATCHING_BOOKS
        || result.relationships_added != MATCHING_BOOKS * 2
        || result.relationships_removed != MATCHING_BOOKS
        || result.tags_created != 2
    {
        return Err("forward bulk relationship counts did not reconcile".into());
    }
    Ok(())
}

fn verify_injected_rollback(
    database: &mut LibraryDatabase,
    query: &LibraryQuery,
    baseline_tag: TagId,
) -> Result<(), String> {
    let snapshot = database.selection_snapshot(query).map_err(display_error)?;
    let selection = BookSelection::all_matching(query.clone(), snapshot.generation, Vec::new());
    if database
        .apply_bulk_tags(
            &selection,
            &BulkTagEdit {
                add: vec![TagReference::New("Invalid\nTag".into())],
                remove: vec![baseline_tag],
            },
        )
        .is_ok()
    {
        return Err("injected invalid tag unexpectedly committed".into());
    }
    let baseline = LibraryQuery {
        facets: ExactFacets::new(Vec::new(), None, vec![baseline_tag], Vec::new())
            .map_err(display_error)?,
        ..LibraryQuery::default()
    };
    if database
        .query_page(&baseline, 0, 1)
        .map_err(display_error)?
        .total
        != MATCHING_BOOKS
    {
        return Err("injected failure partially removed baseline relationships".into());
    }
    Ok(())
}

fn exact_tag_id(database: &LibraryDatabase, name: &str) -> Result<TagId, String> {
    database
        .autocomplete_tags(name, &[], 50)
        .map_err(display_error)?
        .into_iter()
        .find(|usage| usage.tag.name == name)
        .map(|usage| usage.tag.id)
        .ok_or_else(|| format!("tag {name:?} was not found after creation"))
}

fn validate_library(path: &Path, books: u64) -> Result<(), String> {
    let connection = Connection::open(path).map_err(display_error)?;
    let stored: i64 = connection
        .query_row("SELECT count(*) FROM books", [], |row| row.get(0))
        .map_err(display_error)?;
    if u64::try_from(stored).map_err(display_error)? != books {
        return Err(format!(
            "bulk fixture contains {stored} books, expected {books}"
        ));
    }
    Ok(())
}

fn library_state(path: &Path) -> Result<LibraryState, String> {
    let connection = Connection::open(path).map_err(display_error)?;
    let metadata = connection
        .query_row(
            "SELECT count(*), coalesce(sum( \
                 length(title) + length(authors) + length(coalesce(series, '')) + \
                 length(coalesce(publisher, '')) + length(coalesce(language, '')) + \
                 length(coalesce(description, '')) \
             ), 0) FROM books",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(display_error)?;
    let assets = connection
        .query_row(
            "SELECT count(*), coalesce(sum(length(path)), 0) FROM book_assets",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(display_error)?;
    Ok((metadata, assets))
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

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(display_error)?;
    fs::write(path, bytes).map_err(display_error)
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
            "bulk.json".into(),
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
