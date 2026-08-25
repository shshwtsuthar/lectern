//! Release-mode bounded vocabulary-manager read workload.

use std::{
    ffi::OsString,
    fs,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use lectern_storage::LibraryDatabase;
use serde::Serialize;

const USAGE: &str = "Usage:
  organisation-vocabulary-read-benchmark --database PATH --output PATH [OPTIONS]

Options:
  --iterations N  Measured manager page loads (default: 40)
  --warmup N      Warmup manager page loads (default: 10)
";
const LIBRARY_BOOKS: u64 = 50_000;
const PAGE_SIZE: u32 = 100;

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
struct ReadResult {
    schema_version: u32,
    kind: &'static str,
    measured_at_unix_ms: u128,
    database_path: String,
    library_books: u64,
    warmup_iterations: usize,
    measured_iterations: usize,
    page_size: u32,
    verified_checks: Vec<&'static str>,
    peak_rss_delta_bytes: u64,
    scenarios: Vec<ReadScenario>,
}

#[derive(Serialize)]
struct ReadScenario {
    name: &'static str,
    successful_operations: usize,
    observed_rows_per_section: usize,
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
    let database = LibraryDatabase::open(&options.database).map_err(display_error)?;
    if database.count().map_err(display_error)? != LIBRARY_BOOKS {
        return Err(format!(
            "vocabulary fixture must contain {LIBRARY_BOOKS} logical books"
        ));
    }
    validate_deep_pages(&database)?;
    let baseline_rss = peak_resident_bytes()?;
    let rounds = options
        .warmup
        .checked_add(options.iterations)
        .ok_or_else(|| "vocabulary read iteration count overflowed".to_owned())?;
    let mut samples = Vec::with_capacity(options.iterations);
    for round in 0..rounds {
        let started = Instant::now();
        let contributors = database
            .search_contributors("", 0, PAGE_SIZE)
            .map_err(display_error)?;
        let series = database
            .search_series("", 0, PAGE_SIZE)
            .map_err(display_error)?;
        let tags = database
            .search_tags("", 0, PAGE_SIZE)
            .map_err(display_error)?;
        let elapsed = started.elapsed();
        if contributors.len() != PAGE_SIZE as usize
            || series.len() != PAGE_SIZE as usize
            || tags.len() != PAGE_SIZE as usize
            || contributors[0].contributor.display_name != "Contributor 00001"
            || series[0].series.name != "Series 0001"
            || tags[0].tag.name != "Tag 001"
            || contributors[0].books != 8
            || series[0].books != 0
            || series[99].books != 20
            || tags[0].books != 800
            || tags[0].saved_searches != 0
        {
            return Err("bounded manager page did not reconcile fixture order and usage".into());
        }
        if round >= options.warmup {
            samples.push(duration_ns(elapsed)?);
        }
    }
    let result = ReadResult {
        schema_version: 1,
        kind: "organisation-vocabulary-read",
        measured_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(display_error)?
            .as_millis(),
        database_path: options.database.display().to_string(),
        library_books: LIBRARY_BOOKS,
        warmup_iterations: options.warmup,
        measured_iterations: options.iterations,
        page_size: PAGE_SIZE,
        verified_checks: vec![
            "bounded_manager_page",
            "stable_normalized_order",
            "usage_counts",
            "deep_page",
        ],
        peak_rss_delta_bytes: peak_resident_bytes()?.saturating_sub(baseline_rss),
        scenarios: vec![ReadScenario {
            name: "manager_search_page",
            successful_operations: rounds,
            observed_rows_per_section: PAGE_SIZE as usize,
            latency_ms: summarize_latency(&samples),
            samples_ns: samples,
        }],
    };
    let bytes = serde_json::to_vec_pretty(&result).map_err(display_error)?;
    fs::write(&options.output, bytes).map_err(display_error)?;
    println!(
        "Measured {} bounded vocabulary manager page loads",
        options.iterations
    );
    Ok(())
}

fn validate_deep_pages(database: &LibraryDatabase) -> Result<(), String> {
    let contributors = database
        .search_contributors("", 19_900, PAGE_SIZE)
        .map_err(display_error)?;
    let series = database
        .search_series("", 2_400, PAGE_SIZE)
        .map_err(display_error)?;
    let tags = database
        .search_tags("", 400, PAGE_SIZE)
        .map_err(display_error)?;
    if contributors.len() != 100
        || contributors[0].contributor.display_name != "Contributor 19901"
        || series.len() != 100
        || series[0].series.name != "Series 2401"
        || tags.len() != 100
        || tags[0].tag.name != "Tag 401"
    {
        return Err("deep vocabulary pages were not stable and bounded".into());
    }
    Ok(())
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
            "reads.json".into(),
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
