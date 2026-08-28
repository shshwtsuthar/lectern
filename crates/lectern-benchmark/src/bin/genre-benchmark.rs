//! Release-mode deterministic fixed-genre detail-read and metadata-save workload.

use std::{
    ffi::OsString,
    fs,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use lectern_core::{
    Book, BookId,
    organisation::{
        BookEdit, ContributorCreditEdit, ContributorReference, Genre, SeriesMembershipEdit,
        SeriesReference, TagReference,
    },
};
use lectern_storage::LibraryDatabase;
use rusqlite::Connection;
use serde::Serialize;

const USAGE: &str = "Usage:
  genre-benchmark --database PATH --output PATH [OPTIONS]

Options:
  --iterations N  Measured operations per scenario (default: 40)
  --warmup N      Warmup operations per scenario (default: 10)
";
const LIBRARY_BOOKS: u64 = 50_000;
const DETAIL_GENRES: usize = 12;

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
struct BenchmarkResult {
    schema_version: u32,
    kind: &'static str,
    measured_at_unix_ms: u128,
    database_path: String,
    library_books: u64,
    catalog_genres: usize,
    detail_genres: usize,
    warmup_iterations: usize,
    measured_iterations: usize,
    verified_checks: Vec<&'static str>,
    peak_rss_delta_bytes: u64,
    scenarios: Vec<Scenario>,
}

#[derive(Serialize)]
struct Scenario {
    name: &'static str,
    successful_operations: usize,
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

fn run(options: &Options) -> Result<(), String> {
    if !options.database.is_file() {
        return Err(format!(
            "genre benchmark database is not a file: {}",
            options.database.display()
        ));
    }
    if options.output.exists() {
        return Err(format!(
            "genre benchmark output already exists: {}",
            options.output.display()
        ));
    }
    if let Some(parent) = options.output.parent() {
        fs::create_dir_all(parent).map_err(display_error)?;
    }

    let database = LibraryDatabase::open(&options.database).map_err(display_error)?;
    if database.count().map_err(display_error)? != LIBRARY_BOOKS {
        return Err(format!(
            "genre fixture must contain {LIBRARY_BOOKS} logical books"
        ));
    }
    drop(database);
    seed_detail_genres(&options.database)?;

    let mut database = LibraryDatabase::open(&options.database).map_err(display_error)?;
    let baseline_rss = peak_resident_bytes()?;
    let rounds = options
        .warmup
        .checked_add(options.iterations)
        .ok_or_else(|| "genre iteration count overflowed".to_owned())?;
    let detail = measure_detail_load(&database, rounds, options.warmup)?;
    let save = measure_save_and_reload(&mut database, rounds, options.warmup)?;
    let result = BenchmarkResult {
        schema_version: 1,
        kind: "genre-performance",
        measured_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(display_error)?
            .as_millis(),
        database_path: options.database.display().to_string(),
        library_books: LIBRARY_BOOKS,
        catalog_genres: Genre::ALL.len(),
        detail_genres: DETAIL_GENRES,
        warmup_iterations: options.warmup,
        measured_iterations: options.iterations,
        verified_checks: vec![
            "fixed_catalog_size",
            "complete_book_genres",
            "catalog_order",
            "atomic_membership_replace",
            "duplicate_memberships_eliminated",
        ],
        peak_rss_delta_bytes: peak_resident_bytes()?.saturating_sub(baseline_rss),
        scenarios: vec![detail, save],
    };
    fs::write(
        &options.output,
        serde_json::to_vec_pretty(&result).map_err(display_error)?,
    )
    .map_err(display_error)?;
    println!(
        "Measured {} fixed-genre operations across two scenarios",
        options.iterations
    );
    Ok(())
}

fn seed_detail_genres(path: &PathBuf) -> Result<(), String> {
    let mut connection = Connection::open(path).map_err(display_error)?;
    let transaction = connection.transaction().map_err(display_error)?;
    transaction
        .execute("DELETE FROM book_genres WHERE book_id IN (1, 2)", [])
        .map_err(display_error)?;
    let mut insert = transaction
        .prepare("INSERT INTO book_genres(book_id, genre) VALUES (1, ?1)")
        .map_err(display_error)?;
    for genre in Genre::ALL.into_iter().take(DETAIL_GENRES) {
        insert.execute([genre.as_str()]).map_err(display_error)?;
    }
    drop(insert);
    transaction.commit().map_err(display_error)
}

fn measure_detail_load(
    database: &LibraryDatabase,
    rounds: usize,
    warmup: usize,
) -> Result<Scenario, String> {
    let expected = Genre::ALL[..DETAIL_GENRES].to_vec();
    measure("book_detail_with_genres", rounds, warmup, || {
        let book = database
            .get_book(BookId::new(1))
            .map_err(display_error)?
            .ok_or_else(|| "genre detail benchmark book is missing".to_owned())?;
        if book.genres != expected {
            return Err("complete book load did not reconcile fixed genres".into());
        }
        Ok(())
    })
}

fn measure_save_and_reload(
    database: &mut LibraryDatabase,
    rounds: usize,
    warmup: usize,
) -> Result<Scenario, String> {
    let book = database
        .get_book(BookId::new(2))
        .map_err(display_error)?
        .ok_or_else(|| "genre save benchmark book is missing".to_owned())?;
    let mut round = 0_usize;
    measure("genre_membership_save_and_reload", rounds, warmup, || {
        let count = if round.is_multiple_of(2) { 8 } else { 9 };
        let mut genres = Genre::ALL[..count].to_vec();
        genres.push(genres[0]);
        let edit = edit_with_genres(&book, genres);
        database.save_book_edit(&edit).map_err(display_error)?;
        let stored = database
            .get_book(book.id)
            .map_err(display_error)?
            .ok_or_else(|| "saved genre benchmark book disappeared".to_owned())?;
        if stored.genres != Genre::ALL[..count] {
            return Err("atomic genre membership replacement did not reconcile".into());
        }
        round += 1;
        Ok(())
    })
}

fn edit_with_genres(book: &Book, genres: Vec<Genre>) -> BookEdit {
    BookEdit {
        id: book.id,
        title: book.title.clone(),
        publisher: book.publisher.clone(),
        publication_date: book.publication_date,
        language: book.language.clone(),
        description: book.description.clone(),
        rating: book.rating,
        contributors: book
            .contributors
            .iter()
            .map(|credit| ContributorCreditEdit {
                contributor: ContributorReference::Existing(credit.contributor.id),
                role: credit.role,
                position: credit.position,
            })
            .collect(),
        series: book
            .series_membership
            .as_ref()
            .map(|membership| SeriesMembershipEdit {
                series: SeriesReference::Existing(membership.series.id),
                index: membership.index,
            }),
        tags: book
            .tags
            .iter()
            .map(|tag| TagReference::Existing(tag.id))
            .collect(),
        genres,
    }
}

fn measure(
    name: &'static str,
    rounds: usize,
    warmup: usize,
    mut operation: impl FnMut() -> Result<(), String>,
) -> Result<Scenario, String> {
    let mut samples = Vec::with_capacity(rounds.saturating_sub(warmup));
    for round in 0..rounds {
        let started = Instant::now();
        operation()?;
        if round >= warmup {
            samples.push(duration_ns(started.elapsed())?);
        }
    }
    Ok(Scenario {
        name,
        successful_operations: rounds,
        latency_ms: summarize_latency(&samples),
        samples_ns: samples,
    })
}

fn summarize_latency(samples: &[u64]) -> LatencySummary {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    LatencySummary {
        minimum: millis(sorted[0]),
        p50: millis(nearest_rank(&sorted, 50)),
        p95: millis(nearest_rank(&sorted, 95)),
        p99: millis(nearest_rank(&sorted, 99)),
        maximum: millis(*sorted.last().expect("samples are non-empty")),
    }
}

fn nearest_rank(sorted: &[u64], percentile: usize) -> u64 {
    let rank = sorted.len().saturating_mul(percentile).div_ceil(100);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn duration_ns(duration: Duration) -> Result<u64, String> {
    u64::try_from(duration.as_nanos()).map_err(display_error)
}

fn millis(nanoseconds: u64) -> f64 {
    Duration::from_nanos(nanoseconds).as_secs_f64() * 1_000.0
}

#[cfg(target_os = "linux")]
fn peak_resident_bytes() -> Result<u64, String> {
    let status = fs::read_to_string("/proc/self/status").map_err(display_error)?;
    let kibibytes = status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.split_whitespace().next())
        .ok_or_else(|| "VmHWM is missing from /proc/self/status".to_owned())?
        .parse::<u64>()
        .map_err(display_error)?;
    kibibytes
        .checked_mul(1_024)
        .ok_or_else(|| "peak resident byte count overflowed".to_owned())
}

#[cfg(not(target_os = "linux"))]
fn peak_resident_bytes() -> Result<u64, String> {
    Ok(0)
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
