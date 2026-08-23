//! Reproducible, exploratory performance measurements for Lectern.

use std::{
    env,
    ffi::OsString,
    fs::{self, File},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use image::{Rgb, RgbImage, codecs::jpeg::JpegEncoder};
use lectern_core::{BookDraft, BookFormat, LibraryQuery, SortOrder};
use lectern_storage::{ImportRecord, LibraryDatabase};
use serde::Serialize;

const DEFAULT_BOOKS: usize = 50_000;
const DEFAULT_ITERATIONS: usize = 100;
const DEFAULT_WARMUP: usize = 10;
const SEED_BATCH_SIZE: usize = 500;
const DEFAULT_SEED: u64 = 20_260_824;

const USAGE: &str = "Lectern exploratory performance harness

Usage:
  lectern-benchmark seed --database PATH --output PATH [OPTIONS]
  lectern-benchmark query --database PATH --output PATH [OPTIONS]

Seed options:
  --books N          Number of deterministic books (default: 50000)
  --seed N           Metadata seed (default: 20260824)
  --cover-every N    Give every Nth book a cover; 0 disables covers (default: 3)
  --replace          Replace an existing benchmark database

Query options:
  --iterations N     Measured iterations per scenario (default: 100)
  --warmup N         Warmup iterations per scenario (default: 10)

Common options:
  -h, --help         Print help";

fn main() -> ExitCode {
    match run(env::args_os().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(args: impl IntoIterator<Item = OsString>) -> Result<(), String> {
    let mut args = Arguments::new(args);
    let Some(command) = args.next_string()? else {
        println!("{USAGE}");
        return Ok(());
    };
    if command == "-h" || command == "--help" {
        println!("{USAGE}");
        return Ok(());
    }

    match command.as_str() {
        "seed" => run_seed(&SeedOptions::parse(&mut args)?),
        "query" => run_query(&QueryOptions::parse(&mut args)?),
        _ => Err(format!(
            "unknown command '{command}'. Run 'lectern-benchmark --help' for usage"
        )),
    }
}

struct Arguments {
    values: std::vec::IntoIter<OsString>,
}

impl Arguments {
    fn new(values: impl IntoIterator<Item = OsString>) -> Self {
        Self {
            values: values.into_iter().collect::<Vec<_>>().into_iter(),
        }
    }

    fn next_string(&mut self) -> Result<Option<String>, String> {
        self.values
            .next()
            .map(|value| {
                value
                    .into_string()
                    .map_err(|value| format!("argument is not valid UTF-8: {}", value.display()))
            })
            .transpose()
    }

    fn require_value(&mut self, option: &str) -> Result<String, String> {
        self.next_string()?
            .ok_or_else(|| format!("{option} requires a value"))
    }
}

#[derive(Debug, Eq, PartialEq)]
struct SeedOptions {
    database: PathBuf,
    output: PathBuf,
    books: usize,
    seed: u64,
    cover_every: usize,
    replace: bool,
}

impl SeedOptions {
    fn parse(args: &mut Arguments) -> Result<Self, String> {
        let mut database = None;
        let mut output = None;
        let mut books = DEFAULT_BOOKS;
        let mut seed = DEFAULT_SEED;
        let mut cover_every = 3;
        let mut replace = false;
        while let Some(option) = args.next_string()? {
            match option.as_str() {
                "--database" => database = Some(PathBuf::from(args.require_value(&option)?)),
                "--output" => output = Some(PathBuf::from(args.require_value(&option)?)),
                "--books" => books = parse_number(&option, &args.require_value(&option)?)?,
                "--seed" => seed = parse_number(&option, &args.require_value(&option)?)?,
                "--cover-every" => {
                    cover_every = parse_number(&option, &args.require_value(&option)?)?;
                }
                "--replace" => replace = true,
                _ => return Err(format!("unknown seed option '{option}'")),
            }
        }
        if books == 0 {
            return Err("--books must be greater than zero".into());
        }
        Ok(Self {
            database: required_path("--database", database)?,
            output: required_path("--output", output)?,
            books,
            seed,
            cover_every,
            replace,
        })
    }
}

#[derive(Debug, Eq, PartialEq)]
struct QueryOptions {
    database: PathBuf,
    output: PathBuf,
    iterations: usize,
    warmup: usize,
}

impl QueryOptions {
    fn parse(args: &mut Arguments) -> Result<Self, String> {
        let mut database = None;
        let mut output = None;
        let mut iterations = DEFAULT_ITERATIONS;
        let mut warmup = DEFAULT_WARMUP;
        while let Some(option) = args.next_string()? {
            match option.as_str() {
                "--database" => database = Some(PathBuf::from(args.require_value(&option)?)),
                "--output" => output = Some(PathBuf::from(args.require_value(&option)?)),
                "--iterations" => {
                    iterations = parse_number(&option, &args.require_value(&option)?)?;
                }
                "--warmup" => warmup = parse_number(&option, &args.require_value(&option)?)?,
                _ => return Err(format!("unknown query option '{option}'")),
            }
        }
        if iterations == 0 {
            return Err("--iterations must be greater than zero".into());
        }
        Ok(Self {
            database: required_path("--database", database)?,
            output: required_path("--output", output)?,
            iterations,
            warmup,
        })
    }
}

fn required_path(option: &str, value: Option<PathBuf>) -> Result<PathBuf, String> {
    value.ok_or_else(|| format!("missing required {option}"))
}

fn parse_number<T>(option: &str, value: &str) -> Result<T, String>
where
    T: std::str::FromStr,
{
    value
        .parse()
        .map_err(|_| format!("{option} expects a non-negative integer, got '{value}'"))
}

#[derive(Serialize)]
struct SeedResult {
    schema_version: u32,
    kind: &'static str,
    measured_at_unix_ms: u128,
    database_path: String,
    requested_books: usize,
    stored_books: u64,
    metadata_seed: u64,
    cover_every: usize,
    covered_books: usize,
    batch_size: usize,
    elapsed_ms: f64,
    database_bytes: u64,
}

fn run_seed(options: &SeedOptions) -> Result<(), String> {
    if options.database.exists() && !options.replace {
        return Err(format!(
            "{} already exists; choose a new benchmark path or pass --replace",
            options.database.display()
        ));
    }
    remove_database_files(&options.database)?;
    let cover = (options.cover_every > 0)
        .then(make_benchmark_cover)
        .transpose()?;
    let started = Instant::now();
    let mut database = LibraryDatabase::open(&options.database).map_err(display_error)?;
    let mut covered_books = 0;

    for start in (0..options.books).step_by(SEED_BATCH_SIZE) {
        let end = (start + SEED_BATCH_SIZE).min(options.books);
        let records = (start..end)
            .map(|index| {
                let has_cover = options.cover_every > 0 && index % options.cover_every == 0;
                covered_books += usize::from(has_cover);
                benchmark_record(
                    index,
                    options.seed,
                    has_cover.then(|| cover.clone()).flatten(),
                )
            })
            .collect::<Vec<_>>();
        database.import_batch(&records).map_err(display_error)?;
    }

    let elapsed = started.elapsed();
    let stored_books = database.count().map_err(display_error)?;
    drop(database);
    let result = SeedResult {
        schema_version: 1,
        kind: "seed",
        measured_at_unix_ms: unix_time_ms()?,
        database_path: options.database.display().to_string(),
        requested_books: options.books,
        stored_books,
        metadata_seed: options.seed,
        cover_every: options.cover_every,
        covered_books,
        batch_size: SEED_BATCH_SIZE,
        elapsed_ms: duration_ms(elapsed),
        database_bytes: fs::metadata(&options.database)
            .map_err(display_error)?
            .len(),
    };
    write_json(&options.output, &result)?;
    println!(
        "Seeded {stored_books} books in {:.1} ms ({})",
        result.elapsed_ms,
        options.database.display()
    );
    Ok(())
}

fn remove_database_files(database: &Path) -> Result<(), String> {
    for path in [
        database.to_path_buf(),
        PathBuf::from(format!("{}-shm", database.display())),
        PathBuf::from(format!("{}-wal", database.display())),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("could not remove {}: {error}", path.display())),
        }
    }
    Ok(())
}

fn benchmark_record(index: usize, seed: u64, cover_thumbnail: Option<Vec<u8>>) -> ImportRecord {
    const ADJECTIVES: [&str; 12] = [
        "Amber", "Brisk", "Copper", "Distant", "Emerald", "Fallow", "Golden", "Hidden", "Indigo",
        "Jovial", "Kindred", "Luminous",
    ];
    const NOUNS: [&str; 12] = [
        "Archive",
        "Beacon",
        "Chronicle",
        "Delta",
        "Engine",
        "Forest",
        "Garden",
        "Harbor",
        "Island",
        "Journey",
        "Kingdom",
        "Labyrinth",
    ];
    const PUBLISHERS: [&str; 8] = [
        "Aster Press",
        "Beacon House",
        "Copperleaf Books",
        "Driftwood Editions",
        "Ember Works",
        "Field Notes",
        "Grove & Co.",
        "Harbor Press",
    ];
    const LANGUAGES: [&str; 5] = ["en", "en-AU", "fr", "de", "es"];

    let mixed = splitmix64(seed.wrapping_add(u64::try_from(index).expect("book index fits u64")));
    let adjective = ADJECTIVES[bounded_index(mixed, ADJECTIVES.len())];
    let noun = NOUNS[bounded_index(mixed >> 8, NOUNS.len())];
    let author = (mixed >> 16) % 1_000;
    let format = if mixed.is_multiple_of(5) {
        BookFormat::Pdf
    } else {
        BookFormat::Epub
    };
    ImportRecord {
        book: BookDraft {
            title: format!("{adjective} {noun} {index:05}"),
            authors: format!("Author {author:04}"),
            series: index
                .is_multiple_of(4)
                .then(|| format!("Series {:03}", (mixed >> 28) % 250)),
            publisher: Some(PUBLISHERS[bounded_index(mixed >> 36, PUBLISHERS.len())].into()),
            language: Some(LANGUAGES[bounded_index(mixed >> 44, LANGUAGES.len())].into()),
            description: Some(format!(
                "Deterministic benchmark publication {index} generated from seed {seed}."
            )),
            format,
            source_path: PathBuf::from(format!(
                "/lectern-benchmark/{seed}/{index:05}.{}",
                format.as_str()
            )),
        },
        cover_thumbnail,
    }
}

fn bounded_index(value: u64, length: usize) -> usize {
    let length = u64::try_from(length).expect("lookup length fits u64");
    usize::try_from(value % length).expect("bounded lookup index fits usize")
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn make_benchmark_cover() -> Result<Vec<u8>, String> {
    let image = RgbImage::from_fn(48, 72, |x, y| {
        let stripe = ((x / 8) + (y / 12)) % 3;
        match stripe {
            0 => Rgb([28, 80, 112]),
            1 => Rgb([214, 153, 76]),
            _ => Rgb([235, 228, 209]),
        }
    });
    let mut encoded = Vec::new();
    JpegEncoder::new_with_quality(&mut encoded, 82)
        .encode_image(&image)
        .map_err(display_error)?;
    Ok(encoded)
}

#[derive(Serialize)]
struct QueryResult {
    schema_version: u32,
    kind: &'static str,
    measured_at_unix_ms: u128,
    database_path: String,
    library_books: u64,
    warmup_iterations: usize,
    measured_iterations: usize,
    scenarios: Vec<QueryScenarioResult>,
}

#[derive(Serialize)]
struct QueryScenarioResult {
    name: &'static str,
    search: &'static str,
    format: Option<&'static str>,
    sort: &'static str,
    result_count: usize,
    latency_ms: LatencySummary,
    samples_ns: Vec<u64>,
}

#[derive(Serialize)]
struct LatencySummary {
    min: f64,
    mean: f64,
    p50: f64,
    p95: f64,
    p99: f64,
    max: f64,
}

struct QueryScenario {
    name: &'static str,
    search: &'static str,
    format: Option<BookFormat>,
    sort: SortOrder,
}

impl QueryScenario {
    fn query(&self) -> LibraryQuery {
        LibraryQuery {
            search: self.search.into(),
            format: self.format,
            sort: self.sort,
        }
    }
}

fn run_query(options: &QueryOptions) -> Result<(), String> {
    let database = LibraryDatabase::open(&options.database).map_err(display_error)?;
    let library_books = database.count().map_err(display_error)?;
    let scenarios = query_scenarios();
    let mut samples = vec![Vec::with_capacity(options.iterations); scenarios.len()];
    let mut result_counts = vec![0; scenarios.len()];

    for round in 0..(options.warmup + options.iterations) {
        for offset in 0..scenarios.len() {
            let index = (offset + round) % scenarios.len();
            let started = Instant::now();
            let books = database
                .query(&scenarios[index].query())
                .map_err(display_error)?;
            let elapsed = started.elapsed();
            result_counts[index] = books.len();
            if round >= options.warmup {
                samples[index].push(duration_ns(elapsed)?);
            }
        }
    }

    let results = scenarios
        .into_iter()
        .zip(samples)
        .zip(result_counts)
        .map(
            |((scenario, samples_ns), result_count)| QueryScenarioResult {
                name: scenario.name,
                search: scenario.search,
                format: scenario.format.map(BookFormat::as_str),
                sort: sort_name(scenario.sort),
                result_count,
                latency_ms: summarize_latency(&samples_ns),
                samples_ns,
            },
        )
        .collect();
    let result = QueryResult {
        schema_version: 1,
        kind: "query",
        measured_at_unix_ms: unix_time_ms()?,
        database_path: options.database.display().to_string(),
        library_books,
        warmup_iterations: options.warmup,
        measured_iterations: options.iterations,
        scenarios: results,
    };
    write_json(&options.output, &result)?;
    println!(
        "Measured {} query scenarios over {} books ({} iterations each)",
        result.scenarios.len(),
        library_books,
        options.iterations
    );
    Ok(())
}

fn query_scenarios() -> Vec<QueryScenario> {
    vec![
        QueryScenario {
            name: "search_title_prefix",
            search: "Amber",
            format: None,
            sort: SortOrder::Title,
        },
        QueryScenario {
            name: "search_author_prefix",
            search: "Author 0042",
            format: None,
            sort: SortOrder::Title,
        },
        QueryScenario {
            name: "filter_epub",
            search: "",
            format: Some(BookFormat::Epub),
            sort: SortOrder::Title,
        },
        QueryScenario {
            name: "sort_title",
            search: "",
            format: None,
            sort: SortOrder::Title,
        },
        QueryScenario {
            name: "sort_author",
            search: "",
            format: None,
            sort: SortOrder::Author,
        },
        QueryScenario {
            name: "sort_recently_added",
            search: "",
            format: None,
            sort: SortOrder::RecentlyAdded,
        },
        QueryScenario {
            name: "search_filter_sort",
            search: "Luminous",
            format: Some(BookFormat::Pdf),
            sort: SortOrder::Author,
        },
    ]
}

fn sort_name(sort: SortOrder) -> &'static str {
    match sort {
        SortOrder::Title => "title",
        SortOrder::Author => "author",
        SortOrder::RecentlyAdded => "recently_added",
    }
}

fn summarize_latency(samples_ns: &[u64]) -> LatencySummary {
    let mut sorted = samples_ns.to_vec();
    sorted.sort_unstable();
    let total_ns = sorted.iter().map(|&value| u128::from(value)).sum::<u128>();
    let sample_count = u128::try_from(sorted.len()).expect("sample count fits u128");
    let mean_ns = u64::try_from(total_ns / sample_count).expect("mean fits u64");
    LatencySummary {
        min: ns_ms(sorted[0]),
        mean: ns_ms(mean_ns),
        p50: ns_ms(nearest_rank(&sorted, 50)),
        p95: ns_ms(nearest_rank(&sorted, 95)),
        p99: ns_ms(nearest_rank(&sorted, 99)),
        max: ns_ms(*sorted.last().expect("non-empty samples")),
    }
}

fn nearest_rank(sorted: &[u64], percentile: usize) -> u64 {
    let rank = (percentile * sorted.len()).div_ceil(100);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn ns_ms(value: u64) -> f64 {
    Duration::from_nanos(value).as_secs_f64() * 1_000.0
}

fn duration_ns(duration: Duration) -> Result<u64, String> {
    u64::try_from(duration.as_nanos()).map_err(|_| "duration exceeds u64 nanoseconds".into())
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn unix_time_ms() -> Result<u128, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(display_error)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(display_error)?;
    }
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
    use std::ffi::OsString;

    use super::{Arguments, QueryOptions, SeedOptions, benchmark_record, nearest_rank, splitmix64};

    fn arguments(values: &[&str]) -> Arguments {
        Arguments::new(values.iter().map(OsString::from))
    }

    #[test]
    fn parses_seed_options() {
        let options = SeedOptions::parse(&mut arguments(&[
            "--database",
            "library.sqlite3",
            "--output",
            "seed.json",
            "--books",
            "123",
            "--seed",
            "9",
            "--cover-every",
            "0",
            "--replace",
        ]))
        .expect("parse seed options");

        assert_eq!(options.books, 123);
        assert_eq!(options.seed, 9);
        assert_eq!(options.cover_every, 0);
        assert!(options.replace);
    }

    #[test]
    fn parses_query_options() {
        let options = QueryOptions::parse(&mut arguments(&[
            "--database",
            "library.sqlite3",
            "--output",
            "queries.json",
            "--iterations",
            "20",
            "--warmup",
            "3",
        ]))
        .expect("parse query options");

        assert_eq!(options.iterations, 20);
        assert_eq!(options.warmup, 3);
    }

    #[test]
    fn seeded_metadata_is_stable_and_varied() {
        let first = benchmark_record(42, 7, None);
        let repeated = benchmark_record(42, 7, None);
        let other = benchmark_record(43, 7, None);

        assert_eq!(first, repeated);
        assert_ne!(first.book.title, other.book.title);
        assert_eq!(splitmix64(7), splitmix64(7));
    }

    #[test]
    fn nearest_rank_uses_observed_samples() {
        let samples = (1..=100).collect::<Vec<_>>();

        assert_eq!(nearest_rank(&samples, 50), 50);
        assert_eq!(nearest_rank(&samples, 95), 95);
        assert_eq!(nearest_rank(&samples, 99), 99);
    }
}
