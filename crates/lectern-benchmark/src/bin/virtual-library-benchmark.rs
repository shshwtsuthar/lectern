//! Release-mode deterministic virtual-library creation, assignment, and detail-read workload.

use std::{
    ffi::OsString,
    fs,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use lectern_core::{BookId, organisation::VirtualLibraryIcon};
use lectern_storage::LibraryDatabase;
use rusqlite::{Connection, params};
use serde::Serialize;

const USAGE: &str = "Usage:
  virtual-library-benchmark --database PATH --output PATH [OPTIONS]

Options:
  --iterations N  Measured operations per scenario (default: 40)
  --warmup N      Warmup operations per scenario (default: 10)
";
const LIBRARY_BOOKS: u64 = 50_000;
const VIRTUAL_LIBRARIES: usize = 2_500;
const DETAIL_MEMBERSHIPS: usize = 20;
const AUTOCOMPLETE_LIMIT: u32 = 50;

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
    virtual_libraries: usize,
    detail_memberships: usize,
    autocomplete_limit: u32,
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
            "virtual-library benchmark database is not a file: {}",
            options.database.display()
        ));
    }
    if options.output.exists() {
        return Err(format!(
            "virtual-library benchmark output already exists: {}",
            options.output.display()
        ));
    }
    if let Some(parent) = options.output.parent() {
        fs::create_dir_all(parent).map_err(display_error)?;
    }

    // Open through the production adapter first so the fixture always targets the current schema.
    let database = LibraryDatabase::open(&options.database).map_err(display_error)?;
    if database.count().map_err(display_error)? != LIBRARY_BOOKS {
        return Err(format!(
            "virtual-library fixture must contain {LIBRARY_BOOKS} logical books"
        ));
    }
    drop(database);
    seed_virtual_libraries(&options.database)?;

    let mut database = LibraryDatabase::open(&options.database).map_err(display_error)?;
    let baseline_rss = peak_resident_bytes()?;
    let rounds = options
        .warmup
        .checked_add(options.iterations)
        .ok_or_else(|| "virtual-library iteration count overflowed".to_owned())?;

    let autocomplete = measure_autocomplete(&database, rounds, options.warmup)?;
    let detail = measure_detail_load(&database, rounds, options.warmup)?;
    let membership = measure_membership(&mut database, rounds, options.warmup)?;
    let creation = measure_creation(&mut database, rounds, options.warmup)?;

    let result = BenchmarkResult {
        schema_version: 1,
        kind: "virtual-library-performance",
        measured_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(display_error)?
            .as_millis(),
        database_path: options.database.display().to_string(),
        library_books: LIBRARY_BOOKS,
        virtual_libraries: VIRTUAL_LIBRARIES,
        detail_memberships: DETAIL_MEMBERSHIPS,
        autocomplete_limit: AUTOCOMPLETE_LIMIT,
        warmup_iterations: options.warmup,
        measured_iterations: options.iterations,
        verified_checks: vec![
            "bounded_autocomplete",
            "selected_libraries_first",
            "derived_book_counts",
            "complete_book_memberships",
            "idempotent_membership",
            "atomic_create_and_assign",
        ],
        peak_rss_delta_bytes: peak_resident_bytes()?.saturating_sub(baseline_rss),
        scenarios: vec![autocomplete, detail, membership, creation],
    };
    fs::write(
        &options.output,
        serde_json::to_vec_pretty(&result).map_err(display_error)?,
    )
    .map_err(display_error)?;
    println!(
        "Measured {} virtual-library operations across four scenarios",
        options.iterations
    );
    Ok(())
}

fn seed_virtual_libraries(path: &PathBuf) -> Result<(), String> {
    let mut connection = Connection::open(path).map_err(display_error)?;
    let transaction = connection.transaction().map_err(display_error)?;
    transaction
        .execute("DELETE FROM virtual_libraries", [])
        .map_err(display_error)?;
    {
        let mut insert = transaction
            .prepare(
                "INSERT INTO virtual_libraries(name, identity_key, description, icon) \
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .map_err(display_error)?;
        for index in 1..=VIRTUAL_LIBRARIES {
            let name = format!("Shelf {index:04}");
            insert
                .execute(params![
                    name,
                    name.to_ascii_lowercase(),
                    format!("Representative virtual library {index:04}"),
                    VirtualLibraryIcon::ALL[index % VirtualLibraryIcon::ALL.len()].as_str(),
                ])
                .map_err(display_error)?;
        }
    }
    {
        let mut insert = transaction
            .prepare(
                "INSERT INTO book_virtual_libraries(book_id, virtual_library_id) VALUES (1, ?1)",
            )
            .map_err(display_error)?;
        for library in 1..=DETAIL_MEMBERSHIPS {
            insert
                .execute([i64::try_from(library).map_err(display_error)?])
                .map_err(display_error)?;
        }
    }
    transaction.commit().map_err(display_error)
}

fn measure_autocomplete(
    database: &LibraryDatabase,
    rounds: usize,
    warmup: usize,
) -> Result<Scenario, String> {
    let selected = (1..=10)
        .map(lectern_core::organisation::VirtualLibraryId::new)
        .collect::<Vec<_>>();
    measure("virtual_library_autocomplete", rounds, warmup, || {
        let libraries = database
            .autocomplete_virtual_libraries("shelf", &selected, AUTOCOMPLETE_LIMIT)
            .map_err(display_error)?;
        if libraries.len() != AUTOCOMPLETE_LIMIT as usize
            || libraries[..selected.len()]
                .iter()
                .map(|library| library.id)
                .ne(selected.iter().copied())
            || libraries[0].books != 1
        {
            return Err("virtual-library autocomplete did not reconcile fixture order".into());
        }
        Ok(())
    })
}

fn measure_detail_load(
    database: &LibraryDatabase,
    rounds: usize,
    warmup: usize,
) -> Result<Scenario, String> {
    measure("book_detail_with_virtual_libraries", rounds, warmup, || {
        let book = database
            .get_book(BookId::new(1))
            .map_err(display_error)?
            .ok_or_else(|| "detail benchmark book is missing".to_owned())?;
        if book.virtual_libraries.len() != DETAIL_MEMBERSHIPS
            || book.virtual_libraries[0].name != "Shelf 0001"
            || book
                .virtual_libraries
                .iter()
                .any(|library| library.books != 1)
        {
            return Err("complete book load did not reconcile virtual memberships".into());
        }
        Ok(())
    })
}

fn measure_membership(
    database: &mut LibraryDatabase,
    rounds: usize,
    warmup: usize,
) -> Result<Scenario, String> {
    let toggle = database
        .create_virtual_library("Toggle shelf", None, VirtualLibraryIcon::Star, None)
        .map_err(display_error)?;
    let mut included = false;
    measure("virtual_library_membership_toggle", rounds, warmup, || {
        included = !included;
        let result = database
            .set_book_virtual_library_membership(BookId::new(2), toggle.id, included)
            .map_err(display_error)?;
        if !result.changed
            || result.included != included
            || result.library.books != u64::from(included)
        {
            return Err("virtual-library membership result did not reconcile".into());
        }
        Ok(())
    })
}

fn measure_creation(
    database: &mut LibraryDatabase,
    rounds: usize,
    warmup: usize,
) -> Result<Scenario, String> {
    let mut round = 0_usize;
    measure("virtual_library_create_and_assign", rounds, warmup, || {
        let book = BookId::new(i64::try_from(round + 3).map_err(display_error)?);
        let library = database
            .create_virtual_library(
                &format!("Created shelf {round:04}"),
                Some("Created through the representative dialog journey."),
                VirtualLibraryIcon::Books,
                Some(book),
            )
            .map_err(display_error)?;
        round += 1;
        if library.books != 1 || library.description.is_none() {
            return Err("atomic virtual-library creation did not assign its book".into());
        }
        Ok(())
    })
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
        let elapsed = started.elapsed();
        if round >= warmup {
            samples.push(duration_ns(elapsed)?);
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

fn millis(nanoseconds: u64) -> f64 {
    Duration::from_nanos(nanoseconds).as_secs_f64() * 1_000.0
}

fn duration_ns(duration: Duration) -> Result<u64, String> {
    u64::try_from(duration.as_nanos()).map_err(display_error)
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
        .checked_mul(1_024)
        .ok_or_else(|| "peak resident byte count overflowed".to_owned())
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
