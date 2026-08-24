//! Reproducible, exploratory performance measurements for Lectern.

use std::{
    env,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::{self, ExitCode},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use image::{Rgb, RgbImage, codecs::jpeg::JpegEncoder};
use lectern_core::{
    AssetHealth, AssetId, AssetStorage, Book, BookAssetDraft, BookDraft, BookFormat,
    BookMetadataDraft, BookSummary, LibraryQuery, SortOrder,
};
use lectern_desktop::export::{
    EXPORT_BUFFER_BYTES, ExportControl, ExportError, OverwritePolicy, export_file,
};
use lectern_import::{
    ImportProgress, ImportSummary, discover_publications, import_paths, validate_publication,
};
use lectern_storage::{BookImport, ImportRecord, LibraryDatabase};
use lopdf::{
    Document, Object, Stream,
    content::{Content, Operation},
    dictionary,
};
use serde::Serialize;

const DEFAULT_BOOKS: usize = 50_000;
const DEFAULT_ITERATIONS: usize = 100;
const DEFAULT_WARMUP: usize = 10;
const QUERY_PAGE_SIZE: u32 = 128;
const SEED_BATCH_SIZE: usize = 500;
const DEFAULT_SEED: u64 = 20_260_824;
const MAX_RECORDED_FAILURES: usize = 200;
const BENCHMARK_COVER_WIDTH: u32 = 320;
const BENCHMARK_COVER_HEIGHT: u32 = 480;
const BENCHMARK_ADDED_AT_UNIX_SECONDS: i64 = 1_700_000_000;
const REMOVAL_SOURCE_CONTENTS: &[u8] = b"Lectern removal benchmark source bytes\n";
const DETACH_SOURCE_CONTENTS: &[u8] = b"Lectern detach benchmark source bytes\n";
const ATTACHMENT_SOURCE_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const EXPORT_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
const EXPORT_SOURCE_MIB: u32 = 256;
const EXPORT_SOURCE_BLOCK_BYTES: usize = 1024 * 1024;
const EXPORT_COLLISION_BYTES: &[u8] = b"existing destination must remain unchanged";

const USAGE: &str = "Lectern exploratory performance harness

Usage:
  lectern-benchmark seed --database PATH --output PATH [OPTIONS]
  lectern-benchmark query --database PATH --output PATH [OPTIONS]
  lectern-benchmark query-page --database PATH --output PATH [OPTIONS]
  lectern-benchmark query-page-covered --database PATH --output PATH [OPTIONS]
  lectern-benchmark remove --database PATH --output PATH [OPTIONS]
  lectern-benchmark detach --database PATH --output PATH [OPTIONS]
  lectern-benchmark attach --database PATH --output PATH [OPTIONS]
  lectern-benchmark replace --database PATH --output PATH [OPTIONS]
  lectern-benchmark export --database PATH --output PATH [OPTIONS]
  lectern-benchmark reimport --database PATH --output PATH [OPTIONS]
  lectern-benchmark import --database PATH --corpus PATH --output PATH [OPTIONS]

Seed options:
  --books N          Number of deterministic books (default: 50000)
  --seed N           Metadata seed (default: 20260824)
  --cover-every N    Give every Nth book a cover; 0 disables covers (default: 3)
  --replace          Replace an existing benchmark database

Query options:
  --iterations N     Measured iterations per scenario (default: 100)
  --warmup N         Warmup iterations per scenario (default: 10)

Remove options:
  --iterations N     Measured remove-and-refresh iterations (default: 100)
  --warmup N         Warmup iterations (default: 10)

Detach options:
  --iterations N     Measured detach-and-refresh iterations (default: 100)
  --warmup N         Warmup iterations (default: 10)

Attach options:
  --iterations N     Measured validate-attach-refresh iterations (default: 100)
  --warmup N         Warmup iterations (default: 10)

Replace options:
  --iterations N     Measured validate-replace-refresh iterations (default: 100)
  --warmup N         Warmup iterations (default: 10)

Export options:
  --iterations N     Measured 256 MiB copies (default: 100)
  --warmup N         Warmup copies (default: 10)

Re-import options:
  --iterations N     Measured known-path re-imports (default: 100)
  --warmup N         Warmup iterations (default: 10)

Import options:
  --replace          Replace an existing benchmark database

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
        "query-page" => run_query_page(&QueryOptions::parse(&mut args)?),
        "query-page-covered" => run_query_page_covered(&QueryOptions::parse(&mut args)?),
        "remove" => run_remove(&QueryOptions::parse(&mut args)?),
        "detach" => run_detach(&QueryOptions::parse(&mut args)?),
        "attach" => run_attach(&QueryOptions::parse(&mut args)?),
        "replace" => run_replace(&QueryOptions::parse(&mut args)?),
        "export" => run_export(&QueryOptions::parse(&mut args)?),
        "reimport" => run_reimport(&QueryOptions::parse(&mut args)?),
        "import" => run_import(&ImportOptions::parse(&mut args)?),
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

#[derive(Debug, Eq, PartialEq)]
struct ImportOptions {
    database: PathBuf,
    corpus: PathBuf,
    output: PathBuf,
    replace: bool,
}

impl ImportOptions {
    fn parse(args: &mut Arguments) -> Result<Self, String> {
        let mut database = None;
        let mut corpus = None;
        let mut output = None;
        let mut replace = false;
        while let Some(option) = args.next_string()? {
            match option.as_str() {
                "--database" => database = Some(PathBuf::from(args.require_value(&option)?)),
                "--corpus" => corpus = Some(PathBuf::from(args.require_value(&option)?)),
                "--output" => output = Some(PathBuf::from(args.require_value(&option)?)),
                "--replace" => replace = true,
                _ => return Err(format!("unknown import option '{option}'")),
            }
        }
        Ok(Self {
            database: required_path("--database", database)?,
            corpus: required_path("--corpus", corpus)?,
            output: required_path("--output", output)?,
            replace,
        })
    }
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

fn ensure_distinct_paths(
    first_label: &str,
    first: &Path,
    second_label: &str,
    second: &Path,
) -> Result<(), String> {
    if comparison_path(first)? == comparison_path(second)? {
        return Err(format!(
            "{first_label} and {second_label} must use different paths: {}",
            first.display()
        ));
    }
    Ok(())
}

fn ensure_outside_directory(
    path_label: &str,
    path: &Path,
    directory_label: &str,
    directory: &Path,
) -> Result<(), String> {
    if directory.is_dir() && comparison_path(path)?.starts_with(comparison_path(directory)?) {
        return Err(format!(
            "{path_label} must not be inside the {directory_label} directory: {}",
            path.display()
        ));
    }
    Ok(())
}

fn comparison_path(path: &Path) -> Result<PathBuf, String> {
    if path.exists() {
        return fs::canonicalize(path).map_err(display_error);
    }
    if let (Some(parent), Some(name)) = (path.parent(), path.file_name())
        && parent.is_dir()
    {
        return fs::canonicalize(parent)
            .map(|parent| parent.join(name))
            .map_err(display_error);
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir().map_err(display_error)?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
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
    fixed_timestamp_unix_seconds: i64,
    cover_every: usize,
    covered_books: usize,
    cover_width_pixels: u32,
    cover_height_pixels: u32,
    cover_encoded_bytes: usize,
    batch_size: usize,
    elapsed_ms: f64,
    database_bytes: u64,
}

fn run_seed(options: &SeedOptions) -> Result<(), String> {
    ensure_distinct_paths("database", &options.database, "output", &options.output)?;
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

    let stored_books = database.count().map_err(display_error)?;
    if stored_books != u64::try_from(options.books).expect("book count fits u64") {
        return Err(format!(
            "seed integrity check failed: requested {} books but stored {stored_books}",
            options.books
        ));
    }
    drop(database);
    normalize_seed_timestamps(&options.database, options.books)?;
    let elapsed = started.elapsed();
    let result = SeedResult {
        schema_version: 1,
        kind: "seed",
        measured_at_unix_ms: unix_time_ms()?,
        database_path: options.database.display().to_string(),
        requested_books: options.books,
        stored_books,
        metadata_seed: options.seed,
        fixed_timestamp_unix_seconds: BENCHMARK_ADDED_AT_UNIX_SECONDS,
        cover_every: options.cover_every,
        covered_books,
        cover_width_pixels: BENCHMARK_COVER_WIDTH,
        cover_height_pixels: BENCHMARK_COVER_HEIGHT,
        cover_encoded_bytes: cover.as_ref().map_or(0, Vec::len),
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
        sqlite_sidecar(database, "-shm"),
        sqlite_sidecar(database, "-wal"),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("could not remove {}: {error}", path.display())),
        }
    }
    Ok(())
}

fn normalize_seed_timestamps(database: &Path, expected_books: usize) -> Result<(), String> {
    let mut connection = rusqlite::Connection::open(database).map_err(display_error)?;
    let transaction = connection.transaction().map_err(display_error)?;
    let updated = transaction
        .execute(
            "UPDATE books SET added_at = ?1, modified_at = ?1",
            [BENCHMARK_ADDED_AT_UNIX_SECONDS],
        )
        .map_err(display_error)?;
    if updated != expected_books {
        return Err(format!(
            "timestamp normalization updated {updated} books; expected {expected_books}"
        ));
    }
    transaction.commit().map_err(display_error)
}

fn sqlite_sidecar(database: &Path, suffix: &str) -> PathBuf {
    let mut path = database.as_os_str().to_os_string();
    path.push(suffix);
    path.into()
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
    let image = RgbImage::from_fn(BENCHMARK_COVER_WIDTH, BENCHMARK_COVER_HEIGHT, |x, y| {
        let stripe = ((x / 40) + (y / 60)) % 3;
        let base: [u8; 3] = match stripe {
            0 => [28, 80, 112],
            1 => [214, 153, 76],
            _ => [235, 228, 209],
        };
        let coarse_x = u64::from(x / 3);
        let coarse_y = u64::from(y / 3);
        let grain = splitmix64((coarse_x << 32) ^ coarse_y ^ DEFAULT_SEED);
        let adjustment = i16::from(u8::try_from(grain & 31).expect("grain is bounded")) - 15;
        Rgb(base.map(|channel| {
            u8::try_from((i16::from(channel) + adjustment).clamp(0, 255))
                .expect("adjusted channel is clamped")
        }))
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
    asset_health: Option<&'static str>,
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
    asset_health: Option<AssetHealth>,
    sort: SortOrder,
}

#[derive(Clone, Copy)]
enum PagePosition {
    First,
    Deep,
}

impl PagePosition {
    const fn as_str(self) -> &'static str {
        match self {
            Self::First => "first",
            Self::Deep => "deep",
        }
    }
}

struct PageQueryScenario {
    name: &'static str,
    search: &'static str,
    format: Option<BookFormat>,
    asset_health: Option<AssetHealth>,
    sort: SortOrder,
    position: PagePosition,
}

impl PageQueryScenario {
    fn query(&self) -> LibraryQuery {
        LibraryQuery {
            search: self.search.into(),
            format: self.format,
            asset_health: self.asset_health,
            sort: self.sort,
        }
    }
}

impl QueryScenario {
    fn query(&self) -> LibraryQuery {
        LibraryQuery {
            search: self.search.into(),
            format: self.format,
            asset_health: self.asset_health,
            sort: self.sort,
        }
    }
}

fn run_query(options: &QueryOptions) -> Result<(), String> {
    ensure_distinct_paths("database", &options.database, "output", &options.output)?;
    if !options.database.is_file() {
        return Err(format!(
            "benchmark database is not a file: {}",
            options.database.display()
        ));
    }
    let database = LibraryDatabase::open(&options.database).map_err(display_error)?;
    let library_books = database.count().map_err(display_error)?;
    if library_books == 0 {
        return Err(format!(
            "benchmark database contains no books: {}",
            options.database.display()
        ));
    }
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
                asset_health: scenario.asset_health.map(AssetHealth::as_str),
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

#[derive(Serialize)]
struct PageQueryResult {
    schema_version: u32,
    kind: &'static str,
    measured_at_unix_ms: u128,
    database_path: String,
    library_books: u64,
    page_size: u32,
    warmup_iterations: usize,
    measured_iterations: usize,
    scenarios: Vec<PageQueryScenarioResult>,
}

#[derive(Serialize)]
struct PageQueryScenarioResult {
    name: &'static str,
    position: &'static str,
    search: &'static str,
    format: Option<&'static str>,
    asset_health: Option<&'static str>,
    sort: &'static str,
    offset: u64,
    total_count: u64,
    result_count: usize,
    latency_ms: LatencySummary,
    samples_ns: Vec<u64>,
}

fn run_query_page(options: &QueryOptions) -> Result<(), String> {
    run_query_page_scenarios(options, page_query_scenarios())
}

fn run_query_page_covered(options: &QueryOptions) -> Result<(), String> {
    run_query_page_scenarios(options, covered_page_query_scenarios())
}

fn run_query_page_scenarios(
    options: &QueryOptions,
    scenarios: Vec<PageQueryScenario>,
) -> Result<(), String> {
    ensure_distinct_paths("database", &options.database, "output", &options.output)?;
    if !options.database.is_file() {
        return Err(format!(
            "benchmark database is not a file: {}",
            options.database.display()
        ));
    }
    let mut database = LibraryDatabase::open(&options.database).map_err(display_error)?;
    let library_books = database.count().map_err(display_error)?;
    if library_books == 0 {
        return Err(format!(
            "benchmark database contains no books: {}",
            options.database.display()
        ));
    }
    let mut samples = vec![Vec::with_capacity(options.iterations); scenarios.len()];
    let mut result_counts = vec![0; scenarios.len()];
    let mut total_counts = vec![0; scenarios.len()];
    let mut page_offsets = vec![0; scenarios.len()];

    for round in 0..(options.warmup + options.iterations) {
        for scenario_offset in 0..scenarios.len() {
            let index = (scenario_offset + round) % scenarios.len();
            let query = scenarios[index].query();
            let started = Instant::now();
            let (total, offset, books) = match scenarios[index].position {
                PagePosition::First => {
                    let page = database
                        .query_page(&query, 0, QUERY_PAGE_SIZE)
                        .map_err(display_error)?;
                    (page.total, page.offset, page.books)
                }
                PagePosition::Deep => {
                    let offset = library_books.saturating_sub(u64::from(QUERY_PAGE_SIZE));
                    let books = database
                        .query_window(&query, offset, QUERY_PAGE_SIZE)
                        .map_err(display_error)?;
                    (library_books, offset, books)
                }
            };
            let elapsed = started.elapsed();
            total_counts[index] = total;
            page_offsets[index] = offset;
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
        .zip(total_counts)
        .zip(page_offsets)
        .map(
            |((((scenario, samples_ns), result_count), total_count), offset)| {
                PageQueryScenarioResult {
                    name: scenario.name,
                    position: scenario.position.as_str(),
                    search: scenario.search,
                    format: scenario.format.map(BookFormat::as_str),
                    asset_health: scenario.asset_health.map(AssetHealth::as_str),
                    sort: sort_name(scenario.sort),
                    offset,
                    total_count,
                    result_count,
                    latency_ms: summarize_latency(&samples_ns),
                    samples_ns,
                }
            },
        )
        .collect();
    let result = PageQueryResult {
        schema_version: 1,
        kind: "query-page",
        measured_at_unix_ms: unix_time_ms()?,
        database_path: options.database.display().to_string(),
        library_books,
        page_size: QUERY_PAGE_SIZE,
        warmup_iterations: options.warmup,
        measured_iterations: options.iterations,
        scenarios: results,
    };
    write_json(&options.output, &result)?;
    println!(
        "Measured {} paged query scenarios over {} books ({} iterations each)",
        result.scenarios.len(),
        library_books,
        options.iterations
    );
    Ok(())
}

#[derive(Serialize)]
struct RemoveResult {
    schema_version: u32,
    kind: &'static str,
    measured_at_unix_ms: u128,
    database_path: String,
    library_books: u64,
    final_library_books: u64,
    page_size: u32,
    warmup_iterations: usize,
    measured_iterations: usize,
    source_files: Vec<String>,
    source_bytes_unchanged: bool,
    scenarios: Vec<RemoveScenarioResult>,
}

#[derive(Serialize)]
struct RemoveScenarioResult {
    name: &'static str,
    successful_removals: usize,
    refreshed_total: u64,
    refreshed_result_count: usize,
    latency_ms: LatencySummary,
    samples_ns: Vec<u64>,
}

fn run_remove(options: &QueryOptions) -> Result<(), String> {
    ensure_distinct_paths("database", &options.database, "output", &options.output)?;
    if !options.database.is_file() {
        return Err(format!(
            "benchmark database is not a file: {}",
            options.database.display()
        ));
    }
    let source_epub = options.output.with_extension("source.epub");
    let source_pdf = options.output.with_extension("source.pdf");
    for source in [&source_epub, &source_pdf] {
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(source)
            .and_then(|mut file| file.write_all(REMOVAL_SOURCE_CONTENTS))
            .map_err(display_error)?;
    }

    let mut database = LibraryDatabase::open(&options.database).map_err(display_error)?;
    let library_books = database.count().map_err(display_error)?;
    if library_books == 0 {
        return Err(format!(
            "benchmark database contains no books: {}",
            options.database.display()
        ));
    }
    let cover = make_benchmark_cover()?;
    let rounds = options.warmup + options.iterations;
    let mut samples_ns = Vec::with_capacity(options.iterations);
    let mut successful_removals = 0;
    let mut refreshed_total = 0;
    let mut refreshed_result_count = 0;

    for round in 0..rounds {
        let (elapsed, page) = remove_and_refresh(
            &mut database,
            round,
            library_books,
            &source_epub,
            &source_pdf,
            cover.clone(),
        )?;
        successful_removals += 1;
        refreshed_total = page.total;
        refreshed_result_count = page.books.len();
        if round >= options.warmup {
            samples_ns.push(duration_ns(elapsed)?);
        }
    }

    let final_library_books = database.count().map_err(display_error)?;
    if final_library_books != library_books {
        return Err(format!(
            "removal benchmark final count mismatch: got {final_library_books}, expected {library_books}"
        ));
    }
    let result = RemoveResult {
        schema_version: 1,
        kind: "remove",
        measured_at_unix_ms: unix_time_ms()?,
        database_path: options.database.display().to_string(),
        library_books,
        final_library_books,
        page_size: QUERY_PAGE_SIZE,
        warmup_iterations: options.warmup,
        measured_iterations: options.iterations,
        source_files: vec![
            source_epub.display().to_string(),
            source_pdf.display().to_string(),
        ],
        source_bytes_unchanged: true,
        scenarios: vec![RemoveScenarioResult {
            name: "remove_book_and_refresh",
            successful_removals,
            refreshed_total,
            refreshed_result_count,
            latency_ms: summarize_latency(&samples_ns),
            samples_ns,
        }],
    };
    write_json(&options.output, &result)?;
    println!(
        "Measured {} remove-and-refresh iterations over {} books",
        options.iterations, library_books
    );
    Ok(())
}

fn remove_and_refresh(
    database: &mut LibraryDatabase,
    round: usize,
    library_books: u64,
    source_epub: &Path,
    source_pdf: &Path,
    cover_thumbnail: Vec<u8>,
) -> Result<(Duration, lectern_core::LibraryPage), String> {
    let record = removal_candidate(round, source_epub, source_pdf, cover_thumbnail);
    let ids = database.import_books(&[record]).map_err(display_error)?;
    let id = *ids
        .first()
        .ok_or_else(|| "removal benchmark did not import its candidate".to_owned())?;
    let expected_with_candidate = library_books
        .checked_add(1)
        .ok_or_else(|| "benchmark book count overflowed".to_owned())?;
    if database.count().map_err(display_error)? != expected_with_candidate {
        return Err("removal benchmark candidate count was not installed".into());
    }

    let started = Instant::now();
    let removed = database.remove_book(id).map_err(display_error)?;
    let page = database
        .query_page(&LibraryQuery::default(), 0, QUERY_PAGE_SIZE)
        .map_err(display_error)?;
    let elapsed = started.elapsed();
    if !removed {
        return Err("removal benchmark could not remove its candidate".into());
    }
    let expected_results = usize::try_from(library_books.min(u64::from(QUERY_PAGE_SIZE)))
        .expect("bounded page count fits usize");
    if page.total != library_books || page.books.len() != expected_results {
        return Err("removal benchmark refreshed page did not reconcile".into());
    }
    if database.get_book(id).map_err(display_error)?.is_some()
        || database.load_cover(id).map_err(display_error)?.is_some()
    {
        return Err("removal benchmark left book data behind".into());
    }
    let search = database
        .query(&LibraryQuery {
            search: format!("lectern removal candidate {round}"),
            ..LibraryQuery::default()
        })
        .map_err(display_error)?;
    if !search.is_empty() {
        return Err("removal benchmark left searchable metadata behind".into());
    }
    for source in [source_epub, source_pdf] {
        if fs::read(source).map_err(display_error)? != REMOVAL_SOURCE_CONTENTS {
            return Err(format!(
                "removal benchmark changed source file {}",
                source.display()
            ));
        }
    }
    Ok((elapsed, page))
}

fn removal_candidate(
    round: usize,
    source_epub: &Path,
    source_pdf: &Path,
    cover_thumbnail: Vec<u8>,
) -> BookImport {
    BookImport {
        book: BookMetadataDraft {
            title: format!("Lectern Removal Candidate {round}"),
            authors: "Benchmark Author".into(),
            series: Some("Removal Regression".into()),
            publisher: Some("Lectern Benchmark".into()),
            language: Some("en".into()),
            description: Some("Deterministic aggregate removed after every measurement.".into()),
        },
        assets: vec![
            BookAssetDraft {
                format: BookFormat::Epub,
                storage: AssetStorage::Reference,
                path: source_epub.into(),
            },
            BookAssetDraft {
                format: BookFormat::Pdf,
                storage: AssetStorage::Reference,
                path: source_pdf.into(),
            },
        ],
        cover_thumbnail: Some(cover_thumbnail),
    }
}

#[derive(Serialize)]
struct DetachResult {
    schema_version: u32,
    kind: &'static str,
    measured_at_unix_ms: u128,
    database_path: String,
    library_books: u64,
    final_library_books: u64,
    page_size: u32,
    warmup_iterations: usize,
    measured_iterations: usize,
    source_files: Vec<String>,
    source_bytes_unchanged: bool,
    metadata_preserved: bool,
    covers_preserved: bool,
    scenarios: Vec<DetachScenarioResult>,
}

#[derive(Serialize)]
struct DetachScenarioResult {
    name: &'static str,
    successful_detaches: usize,
    refreshed_total: u64,
    refreshed_result_count: usize,
    format_total: u64,
    format_result_count: usize,
    latency_ms: LatencySummary,
    samples_ns: Vec<u64>,
}

fn run_detach(options: &QueryOptions) -> Result<(), String> {
    ensure_distinct_paths("database", &options.database, "output", &options.output)?;
    if !options.database.is_file() {
        return Err(format!(
            "benchmark database is not a file: {}",
            options.database.display()
        ));
    }
    let [source_epub, source_pdf] = prepare_detach_sources(&options.output)?;

    let mut database = LibraryDatabase::open(&options.database).map_err(display_error)?;
    let library_books = database.count().map_err(display_error)?;
    if library_books == 0 {
        return Err(format!(
            "benchmark database contains no books: {}",
            options.database.display()
        ));
    }
    let initial_pdf_books = database
        .query_page(
            &LibraryQuery {
                format: Some(BookFormat::Pdf),
                ..LibraryQuery::default()
            },
            0,
            QUERY_PAGE_SIZE,
        )
        .map_err(display_error)?
        .total;
    let cover = make_benchmark_cover()?;
    let rounds = options.warmup + options.iterations;
    let mut samples_ns = Vec::with_capacity(options.iterations);
    let mut successful_detaches = 0;
    let mut refreshed_total = 0;
    let mut refreshed_result_count = 0;
    let mut format_total = 0;
    let mut format_result_count = 0;

    for round in 0..rounds {
        let measurement = detach_and_refresh(
            &mut database,
            round,
            library_books,
            initial_pdf_books,
            &source_epub,
            &source_pdf,
            cover.clone(),
        )?;
        successful_detaches += 1;
        refreshed_total = measurement.first_page.total;
        refreshed_result_count = measurement.first_page.books.len();
        format_total = measurement.format_page.total;
        format_result_count = measurement.format_page.books.len();
        if round >= options.warmup {
            samples_ns.push(duration_ns(measurement.elapsed)?);
        }
    }

    let final_library_books = database.count().map_err(display_error)?;
    if final_library_books != library_books {
        return Err(format!(
            "detach benchmark final count mismatch: got {final_library_books}, expected {library_books}"
        ));
    }
    let source_bytes_unchanged = [&source_epub, &source_pdf]
        .into_iter()
        .all(|source| fs::read(source).is_ok_and(|bytes| bytes == DETACH_SOURCE_CONTENTS));
    let result = DetachResult {
        schema_version: 1,
        kind: "detach",
        measured_at_unix_ms: unix_time_ms()?,
        database_path: options.database.display().to_string(),
        library_books,
        final_library_books,
        page_size: QUERY_PAGE_SIZE,
        warmup_iterations: options.warmup,
        measured_iterations: options.iterations,
        source_files: vec![
            source_epub.display().to_string(),
            source_pdf.display().to_string(),
        ],
        source_bytes_unchanged,
        metadata_preserved: true,
        covers_preserved: true,
        scenarios: vec![DetachScenarioResult {
            name: "detach_asset_and_refresh",
            successful_detaches,
            refreshed_total,
            refreshed_result_count,
            format_total,
            format_result_count,
            latency_ms: summarize_latency(&samples_ns),
            samples_ns,
        }],
    };
    write_json(&options.output, &result)?;
    println!(
        "Measured {} detach-and-refresh iterations over {} books",
        options.iterations, library_books
    );
    Ok(())
}

fn prepare_detach_sources(output: &Path) -> Result<[PathBuf; 2], String> {
    let sources = [
        output.with_extension("detach-source.epub"),
        output.with_extension("detach-source.pdf"),
    ];
    for source in &sources {
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(source)
            .and_then(|mut file| file.write_all(DETACH_SOURCE_CONTENTS))
            .map_err(display_error)?;
    }
    Ok(sources)
}

struct DetachMeasurement {
    elapsed: Duration,
    first_page: lectern_core::LibraryPage,
    format_page: lectern_core::LibraryPage,
}

#[allow(clippy::too_many_arguments)]
fn detach_and_refresh(
    database: &mut LibraryDatabase,
    round: usize,
    library_books: u64,
    initial_pdf_books: u64,
    source_epub: &Path,
    source_pdf: &Path,
    cover_thumbnail: Vec<u8>,
) -> Result<DetachMeasurement, String> {
    let record = removal_candidate(round, source_epub, source_pdf, cover_thumbnail);
    let id = database
        .import_books(&[record])
        .map_err(display_error)?
        .into_iter()
        .next()
        .ok_or_else(|| "detach benchmark did not import its candidate".to_owned())?;
    let original = database
        .get_book(id)
        .map_err(display_error)?
        .ok_or_else(|| format!("detach candidate {id} disappeared"))?;
    let detached = original
        .assets
        .iter()
        .find(|asset| asset.format == BookFormat::Pdf)
        .ok_or_else(|| format!("detach candidate {id} did not have two formats"))?
        .id;

    let started = Instant::now();
    let owner = database.detach_asset(detached).map_err(display_error)?;
    let first_page = database
        .query_page(
            &LibraryQuery {
                sort: SortOrder::RecentlyAdded,
                ..LibraryQuery::default()
            },
            0,
            QUERY_PAGE_SIZE,
        )
        .map_err(display_error)?;
    let format_page = database
        .query_page(
            &LibraryQuery {
                format: Some(BookFormat::Pdf),
                ..LibraryQuery::default()
            },
            0,
            QUERY_PAGE_SIZE,
        )
        .map_err(display_error)?;
    let elapsed = started.elapsed();

    if owner != id
        || first_page.total != library_books + 1
        || format_page.total != initial_pdf_books
    {
        return Err("detach benchmark refresh did not reconcile".into());
    }
    let updated = database
        .get_book(id)
        .map_err(display_error)?
        .ok_or_else(|| format!("detached book {id} disappeared"))?;
    if updated.title != original.title
        || updated.authors != original.authors
        || updated.series != original.series
        || updated.assets.len() != 1
        || updated.assets[0].format != BookFormat::Epub
        || updated.assets.iter().any(|asset| asset.id == detached)
    {
        return Err(format!("detach changed book data for {id}"));
    }
    if database.load_cover(id).map_err(display_error)?.is_none() {
        return Err(format!("detach removed the cover for {id}"));
    }
    for source in [source_epub, source_pdf] {
        if fs::read(source).map_err(display_error)? != DETACH_SOURCE_CONTENTS {
            return Err(format!("detach changed source file {}", source.display()));
        }
    }
    if !database.remove_book(id).map_err(display_error)? {
        return Err(format!("detach benchmark could not clean up book {id}"));
    }
    Ok(DetachMeasurement {
        elapsed,
        first_page,
        format_page,
    })
}

#[derive(Serialize)]
struct AttachResult {
    schema_version: u32,
    kind: &'static str,
    measured_at_unix_ms: u128,
    database_path: String,
    library_books: u64,
    final_library_books: u64,
    initial_pdf_books: u64,
    final_pdf_books: u64,
    page_size: u32,
    source_payload_bytes: usize,
    minimum_source_bytes: u64,
    maximum_source_bytes: u64,
    warmup_iterations: usize,
    measured_iterations: usize,
    source_files: Vec<String>,
    source_bytes_unchanged: bool,
    metadata_preserved: bool,
    covers_preserved: bool,
    scenarios: Vec<AttachScenarioResult>,
}

#[derive(Serialize)]
struct AttachScenarioResult {
    name: &'static str,
    validated_publications: usize,
    successful_attachments: usize,
    refreshed_total: u64,
    refreshed_result_count: usize,
    latency_ms: LatencySummary,
    samples_ns: Vec<u64>,
}

struct PreparedAttachmentSources {
    files: Vec<PathBuf>,
    fingerprint: FileFingerprint,
}

struct AttachmentMeasurements {
    validated_publications: usize,
    successful_attachments: usize,
    refreshed_total: u64,
    refreshed_result_count: usize,
    samples_ns: Vec<u64>,
}

fn run_attach(options: &QueryOptions) -> Result<(), String> {
    ensure_distinct_paths("database", &options.database, "output", &options.output)?;
    if !options.database.is_file() {
        return Err(format!(
            "benchmark database is not a file: {}",
            options.database.display()
        ));
    }

    let rounds = options
        .warmup
        .checked_add(options.iterations)
        .ok_or_else(|| "attachment iteration count overflowed".to_owned())?;
    let sources = prepare_attachment_sources(&options.output, rounds)?;

    let mut database = LibraryDatabase::open(&options.database).map_err(display_error)?;
    let library_books = database.count().map_err(display_error)?;
    if library_books == 0 {
        return Err(format!(
            "benchmark database contains no books: {}",
            options.database.display()
        ));
    }
    let candidates = covered_epub_candidates(&database, rounds)?;
    let initial_pdf_books = database
        .query_page(
            &LibraryQuery {
                format: Some(BookFormat::Pdf),
                ..LibraryQuery::default()
            },
            0,
            0,
        )
        .map_err(display_error)?
        .total;

    let measurements = measure_attachments(
        &mut database,
        &candidates,
        &sources,
        initial_pdf_books,
        options.warmup,
        options.iterations,
    )?;

    let final_library_books = database.count().map_err(display_error)?;
    if final_library_books != library_books {
        return Err(format!(
            "attachment benchmark changed book count: got {final_library_books}, expected {library_books}"
        ));
    }
    let final_pdf_books = initial_pdf_books
        .checked_add(u64::try_from(rounds).expect("attachment count fits u64"))
        .ok_or_else(|| "attachment final PDF count overflowed".to_owned())?;
    if measurements.refreshed_total != final_pdf_books {
        return Err("attachment benchmark final PDF count did not reconcile".into());
    }

    let result = AttachResult {
        schema_version: 1,
        kind: "attach",
        measured_at_unix_ms: unix_time_ms()?,
        database_path: options.database.display().to_string(),
        library_books,
        final_library_books,
        initial_pdf_books,
        final_pdf_books,
        page_size: QUERY_PAGE_SIZE,
        source_payload_bytes: ATTACHMENT_SOURCE_PAYLOAD_BYTES,
        minimum_source_bytes: sources.fingerprint.bytes,
        maximum_source_bytes: sources.fingerprint.bytes,
        warmup_iterations: options.warmup,
        measured_iterations: options.iterations,
        source_files: sources
            .files
            .iter()
            .map(|source| source.display().to_string())
            .collect(),
        source_bytes_unchanged: true,
        metadata_preserved: true,
        covers_preserved: true,
        scenarios: vec![AttachScenarioResult {
            name: "attach_validated_format_and_refresh",
            validated_publications: measurements.validated_publications,
            successful_attachments: measurements.successful_attachments,
            refreshed_total: measurements.refreshed_total,
            refreshed_result_count: measurements.refreshed_result_count,
            latency_ms: summarize_latency(&measurements.samples_ns),
            samples_ns: measurements.samples_ns,
        }],
    };
    write_json(&options.output, &result)?;
    println!(
        "Measured {} validate-attach-refresh iterations over {} books",
        options.iterations, library_books
    );
    Ok(())
}

fn prepare_attachment_sources(
    output: &Path,
    rounds: usize,
) -> Result<PreparedAttachmentSources, String> {
    let directory = output.with_extension("sources");
    fs::create_dir(&directory).map_err(display_error)?;
    let prototype = directory.join("prototype.pdf");
    create_attachment_pdf(&prototype, ATTACHMENT_SOURCE_PAYLOAD_BYTES)?;
    validate_publication(&prototype, BookFormat::Pdf).map_err(display_error)?;
    let fingerprint = fingerprint_file(&prototype)?;

    let mut files = Vec::with_capacity(rounds);
    for round in 0..rounds {
        let source = directory.join(format!("attachment-{round:04}.pdf"));
        if fs::hard_link(&prototype, &source).is_err() {
            fs::copy(&prototype, &source).map_err(display_error)?;
        }
        if fingerprint_file(&source)? != fingerprint {
            return Err(format!(
                "attachment benchmark source copy did not reconcile: {}",
                source.display()
            ));
        }
        files.push(source);
    }
    Ok(PreparedAttachmentSources { files, fingerprint })
}

fn covered_epub_candidates(
    database: &LibraryDatabase,
    rounds: usize,
) -> Result<Vec<BookSummary>, String> {
    let candidates = database
        .query(&LibraryQuery {
            format: Some(BookFormat::Epub),
            ..LibraryQuery::default()
        })
        .map_err(display_error)?
        .into_iter()
        .filter(|book| book.has_cover)
        .take(rounds)
        .collect::<Vec<_>>();
    if candidates.len() != rounds {
        return Err(format!(
            "attachment benchmark needs {rounds} covered EPUB books but found {}",
            candidates.len()
        ));
    }
    Ok(candidates)
}

fn measure_attachments(
    database: &mut LibraryDatabase,
    candidates: &[BookSummary],
    sources: &PreparedAttachmentSources,
    initial_pdf_books: u64,
    warmup: usize,
    measured: usize,
) -> Result<AttachmentMeasurements, String> {
    let mut samples_ns = Vec::with_capacity(measured);
    let mut refreshed_total = initial_pdf_books;
    let mut refreshed_result_count = 0;
    for (round, (candidate, source)) in candidates.iter().zip(&sources.files).enumerate() {
        let expected_pdf_books = initial_pdf_books
            .checked_add(u64::try_from(round + 1).expect("attachment count fits u64"))
            .ok_or_else(|| "attachment PDF count overflowed".to_owned())?;
        let (elapsed, result_count) = attach_and_refresh(
            database,
            candidate,
            source,
            sources.fingerprint,
            expected_pdf_books,
        )?;
        refreshed_total = expected_pdf_books;
        refreshed_result_count = result_count;
        if round >= warmup {
            samples_ns.push(duration_ns(elapsed)?);
        }
    }
    let successful_attachments = warmup
        .checked_add(measured)
        .ok_or_else(|| "attachment count overflowed".to_owned())?;
    Ok(AttachmentMeasurements {
        validated_publications: successful_attachments,
        successful_attachments,
        refreshed_total,
        refreshed_result_count,
        samples_ns,
    })
}

fn attach_and_refresh(
    database: &mut LibraryDatabase,
    candidate: &BookSummary,
    source: &Path,
    expected_source: FileFingerprint,
    expected_pdf_books: u64,
) -> Result<(Duration, usize), String> {
    let original = database
        .get_book(candidate.id)
        .map_err(display_error)?
        .ok_or_else(|| format!("attachment candidate {} disappeared", candidate.id))?;
    if original.assets.len() != 1 || original.assets[0].format != BookFormat::Epub {
        return Err(format!(
            "attachment candidate {} was not EPUB-only",
            candidate.id
        ));
    }
    let original_cover = database
        .load_cover(candidate.id)
        .map_err(display_error)?
        .ok_or_else(|| format!("attachment candidate {} lost its cover", candidate.id))?;

    let started = Instant::now();
    validate_publication(source, BookFormat::Pdf).map_err(display_error)?;
    let attached = database
        .attach_reference_asset(candidate.id, BookFormat::Pdf, source)
        .map_err(display_error)?;
    let page = database
        .query_page(
            &LibraryQuery {
                format: Some(BookFormat::Pdf),
                ..LibraryQuery::default()
            },
            0,
            QUERY_PAGE_SIZE,
        )
        .map_err(display_error)?;
    let elapsed = started.elapsed();

    let expected_page = usize::try_from(expected_pdf_books.min(u64::from(QUERY_PAGE_SIZE)))
        .expect("bounded page count fits usize");
    if page.total != expected_pdf_books || page.books.len() != expected_page {
        return Err(format!(
            "attachment refresh did not reconcile for book {}",
            candidate.id
        ));
    }
    validate_attached_book(
        database,
        candidate,
        source,
        attached,
        &original,
        &original_cover,
    )?;
    if fingerprint_file(source)? != expected_source {
        return Err(format!(
            "attachment changed source file {}",
            source.display()
        ));
    }
    Ok((elapsed, page.books.len()))
}

fn validate_attached_book(
    database: &LibraryDatabase,
    candidate: &BookSummary,
    source: &Path,
    attached: AssetId,
    original: &Book,
    original_cover: &[u8],
) -> Result<(), String> {
    let updated = database
        .get_book(candidate.id)
        .map_err(display_error)?
        .ok_or_else(|| format!("attached book {} disappeared", candidate.id))?;
    if !book_metadata_matches(original, &updated)
        || updated.assets.len() != 2
        || updated.assets[0] != original.assets[0]
    {
        return Err(format!(
            "attachment changed metadata or existing assets for book {}",
            candidate.id
        ));
    }
    let attached_asset = updated
        .assets
        .iter()
        .find(|asset| asset.id == attached)
        .ok_or_else(|| format!("attached asset {attached} was not stored"))?;
    if attached_asset.format != BookFormat::Pdf
        || attached_asset.storage != AssetStorage::Reference
        || attached_asset.health != AssetHealth::Available
        || attached_asset.path != source
    {
        return Err(format!("attached asset {attached} did not reconcile"));
    }
    if database
        .load_cover(candidate.id)
        .map_err(display_error)?
        .as_deref()
        != Some(original_cover)
    {
        return Err(format!(
            "attachment changed the cover for book {}",
            candidate.id
        ));
    }
    let matching = database
        .query(&LibraryQuery {
            search: original.title.clone(),
            format: Some(BookFormat::Pdf),
            ..LibraryQuery::default()
        })
        .map_err(display_error)?;
    if matching.len() != 1 || matching[0].id != candidate.id {
        return Err(format!(
            "attached book {} was not uniquely discoverable",
            candidate.id
        ));
    }
    Ok(())
}

fn create_attachment_pdf(path: &Path, payload_bytes: usize) -> Result<(), String> {
    let mut document = Document::with_version("1.5");
    let page_tree_id = document.new_object_id();
    let content = Content {
        operations: vec![
            Operation::new("q", vec![]),
            Operation::new("rg", vec![0.09.into(), 0.31.into(), 0.55.into()]),
            Operation::new("re", vec![0.into(), 0.into(), 300.into(), 450.into()]),
            Operation::new("f", vec![]),
            Operation::new("Q", vec![]),
        ],
    };
    let content_id = document.add_object(Stream::new(
        dictionary! {},
        content.encode().map_err(display_error)?,
    ));
    let payload_id = document.add_object(Stream::new(dictionary! {}, vec![b'L'; payload_bytes]));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => page_tree_id,
        "Contents" => content_id,
        "MediaBox" => vec![0.into(), 0.into(), 300.into(), 450.into()],
        "Resources" => dictionary! {},
    });
    document.objects.insert(
        page_tree_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![page_id.into()],
            "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => page_tree_id,
        "LecternBenchmarkPayload" => payload_id,
    });
    document.trailer.set("Root", catalog_id);
    document.save(path).map_err(display_error)?;
    Ok(())
}

fn book_metadata_matches(original: &Book, updated: &Book) -> bool {
    original.id == updated.id
        && original.title == updated.title
        && original.authors == updated.authors
        && original.series == updated.series
        && original.publisher == updated.publisher
        && original.language == updated.language
        && original.description == updated.description
}

#[derive(Serialize)]
struct ReplaceResult {
    schema_version: u32,
    kind: &'static str,
    measured_at_unix_ms: u128,
    database_path: String,
    library_books: u64,
    final_library_books: u64,
    page_size: u32,
    source_payload_bytes: usize,
    source_files: Vec<String>,
    verified_checks: [&'static str; 4],
    warmup_iterations: usize,
    measured_iterations: usize,
    scenarios: Vec<ReplaceScenarioResult>,
}

#[derive(Serialize)]
struct ReplaceScenarioResult {
    name: &'static str,
    validated_publications: usize,
    successful_replacements: usize,
    refreshed_total: u64,
    refreshed_result_count: usize,
    latency_ms: LatencySummary,
    samples_ns: Vec<u64>,
}

fn run_replace(options: &QueryOptions) -> Result<(), String> {
    ensure_distinct_paths("database", &options.database, "output", &options.output)?;
    if !options.database.is_file() {
        return Err(format!(
            "benchmark database is not a file: {}",
            options.database.display()
        ));
    }
    let [original_source, replacement_source] = prepare_replacement_sources(&options.output)?;
    let original_fingerprint = fingerprint_file(&original_source)?;
    let replacement_fingerprint = fingerprint_file(&replacement_source)?;
    let mut database = LibraryDatabase::open(&options.database).map_err(display_error)?;
    let library_books = database.count().map_err(display_error)?;
    if library_books == 0 {
        return Err(format!(
            "benchmark database contains no books: {}",
            options.database.display()
        ));
    }
    let cover = make_benchmark_cover()?;
    let rounds = options.warmup + options.iterations;
    let mut samples_ns = Vec::with_capacity(options.iterations);
    let mut refreshed_total = 0;
    let mut refreshed_result_count = 0;

    for round in 0..rounds {
        let measurement = replace_and_refresh(
            &mut database,
            round,
            library_books,
            &original_source,
            &replacement_source,
            original_fingerprint,
            replacement_fingerprint,
            cover.clone(),
        )?;
        refreshed_total = measurement.1;
        refreshed_result_count = measurement.2;
        if round >= options.warmup {
            samples_ns.push(duration_ns(measurement.0)?);
        }
    }

    let final_library_books = database.count().map_err(display_error)?;
    if final_library_books != library_books {
        return Err(format!(
            "replacement benchmark final count mismatch: got {final_library_books}, expected {library_books}"
        ));
    }
    let result = ReplaceResult {
        schema_version: 1,
        kind: "replace",
        measured_at_unix_ms: unix_time_ms()?,
        database_path: options.database.display().to_string(),
        library_books,
        final_library_books,
        page_size: QUERY_PAGE_SIZE,
        source_payload_bytes: ATTACHMENT_SOURCE_PAYLOAD_BYTES,
        source_files: vec![
            original_source.display().to_string(),
            replacement_source.display().to_string(),
        ],
        verified_checks: ["source_bytes", "metadata", "covers", "asset_identity"],
        warmup_iterations: options.warmup,
        measured_iterations: options.iterations,
        scenarios: vec![ReplaceScenarioResult {
            name: "replace_validated_asset_and_refresh",
            validated_publications: rounds,
            successful_replacements: rounds,
            refreshed_total,
            refreshed_result_count,
            latency_ms: summarize_latency(&samples_ns),
            samples_ns,
        }],
    };
    write_json(&options.output, &result)?;
    println!(
        "Measured {} validate-replace-refresh iterations over {} books",
        options.iterations, library_books
    );
    Ok(())
}

fn prepare_replacement_sources(output: &Path) -> Result<[PathBuf; 2], String> {
    let sources = [
        output.with_extension("replacement-original.pdf"),
        output.with_extension("replacement-new.pdf"),
    ];
    for source in &sources {
        create_attachment_pdf(source, ATTACHMENT_SOURCE_PAYLOAD_BYTES)?;
    }
    Ok(sources)
}

#[allow(clippy::too_many_arguments)]
fn replace_and_refresh(
    database: &mut LibraryDatabase,
    round: usize,
    library_books: u64,
    original_source: &Path,
    replacement_source: &Path,
    original_fingerprint: FileFingerprint,
    replacement_fingerprint: FileFingerprint,
    cover_thumbnail: Vec<u8>,
) -> Result<(Duration, u64, usize), String> {
    let id = database
        .import_books(&[replacement_candidate(
            round,
            original_source,
            cover_thumbnail,
        )])
        .map_err(display_error)?
        .into_iter()
        .next()
        .ok_or_else(|| "replacement benchmark did not import its candidate".to_owned())?;
    let original = database
        .get_book(id)
        .map_err(display_error)?
        .ok_or_else(|| format!("replacement candidate {id} disappeared"))?;
    let asset_id = original.assets[0].id;
    let original_cover = database
        .load_cover(id)
        .map_err(display_error)?
        .ok_or_else(|| format!("replacement candidate {id} lost its cover"))?;

    let started = Instant::now();
    validate_publication(replacement_source, BookFormat::Pdf).map_err(display_error)?;
    database
        .replace_reference_asset(asset_id, replacement_source, BookFormat::Pdf)
        .map_err(display_error)?;
    let page = database
        .query_page(
            &LibraryQuery {
                sort: SortOrder::RecentlyAdded,
                ..LibraryQuery::default()
            },
            0,
            QUERY_PAGE_SIZE,
        )
        .map_err(display_error)?;
    let elapsed = started.elapsed();

    let expected_total = library_books + 1;
    let expected_page = usize::try_from(expected_total.min(u64::from(QUERY_PAGE_SIZE)))
        .expect("bounded page count fits usize");
    if page.total != expected_total || page.books.len() != expected_page {
        return Err(format!(
            "replacement refresh did not reconcile for book {id}"
        ));
    }
    let updated = database
        .get_book(id)
        .map_err(display_error)?
        .ok_or_else(|| format!("replaced book {id} disappeared"))?;
    if !book_metadata_matches(&original, &updated)
        || updated.assets.len() != 1
        || updated.assets[0].id != asset_id
        || updated.assets[0].format != BookFormat::Pdf
        || updated.assets[0].storage != AssetStorage::Reference
        || updated.assets[0].health != AssetHealth::Available
        || updated.assets[0].path != replacement_source
    {
        return Err(format!(
            "replacement changed book identity or metadata for {id}"
        ));
    }
    if database.load_cover(id).map_err(display_error)?.as_deref() != Some(&original_cover) {
        return Err(format!("replacement changed the cover for book {id}"));
    }
    if fingerprint_file(original_source)? != original_fingerprint
        || fingerprint_file(replacement_source)? != replacement_fingerprint
    {
        return Err("replacement changed original or replacement source bytes".into());
    }
    if !database.remove_book(id).map_err(display_error)? {
        return Err(format!(
            "replacement benchmark could not clean up book {id}"
        ));
    }
    Ok((elapsed, page.total, page.books.len()))
}

fn replacement_candidate(round: usize, source: &Path, cover_thumbnail: Vec<u8>) -> BookImport {
    BookImport {
        book: BookMetadataDraft {
            title: format!("Lectern Replacement Candidate {round}"),
            authors: "Benchmark Author".into(),
            series: Some("Replacement Regression".into()),
            publisher: Some("Lectern Benchmark".into()),
            language: Some("en".into()),
            description: Some("Deterministic asset replaced after validation.".into()),
        },
        assets: vec![BookAssetDraft {
            format: BookFormat::Pdf,
            storage: AssetStorage::Reference,
            path: source.into(),
        }],
        cover_thumbnail: Some(cover_thumbnail),
    }
}

#[derive(Serialize)]
struct ReimportResult {
    schema_version: u32,
    kind: &'static str,
    measured_at_unix_ms: u128,
    database_path: String,
    library_books: u64,
    final_library_books: u64,
    warmup_iterations: usize,
    measured_iterations: usize,
    metadata_preserved: bool,
    assets_preserved: bool,
    covers_preserved: bool,
    scenarios: Vec<ReimportScenarioResult>,
}

#[derive(Serialize)]
struct ReimportScenarioResult {
    name: &'static str,
    successful_reimports: usize,
    latency_ms: LatencySummary,
    samples_ns: Vec<u64>,
}

struct ReimportCandidate {
    original: Book,
    original_cover: Vec<u8>,
    incoming: BookImport,
}

#[derive(Serialize)]
struct ExportResult {
    schema_version: u32,
    kind: &'static str,
    measured_at_unix_ms: u128,
    database_path: String,
    library_books: u64,
    source_path: String,
    source_bytes: u64,
    copy_buffer_bytes: usize,
    warmup_iterations: usize,
    measured_iterations: usize,
    baseline_rss_bytes: Option<u64>,
    peak_rss_bytes: Option<u64>,
    peak_rss_delta_bytes: Option<u64>,
    verified_checks: Vec<&'static str>,
    scenarios: Vec<ExportScenarioResult>,
}

#[derive(Serialize)]
struct ExportScenarioResult {
    name: &'static str,
    successful_exports: usize,
    latency_ms: LatencySummary,
    samples_ns: Vec<u64>,
    copy_latency_ms: LatencySummary,
    copy_samples_ns: Vec<u64>,
    throughput_mib_per_second: ThroughputSummary,
}

#[derive(Serialize)]
struct ThroughputSummary {
    min: f64,
    p05: f64,
    mean: f64,
    p50: f64,
    max: f64,
}

struct ExportMeasurements {
    first_progress_samples: Vec<u64>,
    copy_samples: Vec<u64>,
    baseline_rss_bytes: Option<u64>,
    peak_rss_bytes: Option<u64>,
}

fn run_export(options: &QueryOptions) -> Result<(), String> {
    ensure_distinct_paths("database", &options.database, "output", &options.output)?;
    let database = LibraryDatabase::open(&options.database).map_err(display_error)?;
    let library_books = database.count().map_err(display_error)?;
    drop(database);

    let workload = options
        .output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| "export output must have a parent directory".to_owned())?
        .join("export-workload");
    fs::create_dir(&workload).map_err(display_error)?;
    let source = workload.join("representative-256-mib.epub");
    write_export_source(&source)?;

    let total_rounds = options.warmup + options.iterations;
    let measurements = measure_exports(options, &workload, &source)?;
    verify_export_failure_cases(&workload, &source)?;
    let throughput_samples = measurements
        .copy_samples
        .iter()
        .map(|sample| {
            let seconds = Duration::from_nanos(*sample).as_secs_f64();
            f64::from(EXPORT_SOURCE_MIB) / seconds
        })
        .collect::<Vec<_>>();
    let peak_rss_delta_bytes = measurements
        .baseline_rss_bytes
        .zip(measurements.peak_rss_bytes)
        .map(|(baseline, peak)| peak.saturating_sub(baseline));
    let result = ExportResult {
        schema_version: 1,
        kind: "export",
        measured_at_unix_ms: unix_time_ms()?,
        database_path: options.database.display().to_string(),
        library_books,
        source_path: source.display().to_string(),
        source_bytes: EXPORT_SOURCE_BYTES,
        copy_buffer_bytes: EXPORT_BUFFER_BYTES,
        warmup_iterations: options.warmup,
        measured_iterations: options.iterations,
        baseline_rss_bytes: measurements.baseline_rss_bytes,
        peak_rss_bytes: measurements.peak_rss_bytes,
        peak_rss_delta_bytes,
        verified_checks: vec![
            "exact_bytes",
            "collision_preserved",
            "missing_source_rejected",
            "temporary_cleanup",
        ],
        scenarios: vec![ExportScenarioResult {
            name: "export_large_file",
            successful_exports: total_rounds,
            latency_ms: summarize_latency(&measurements.first_progress_samples),
            samples_ns: measurements.first_progress_samples,
            copy_latency_ms: summarize_latency(&measurements.copy_samples),
            copy_samples_ns: measurements.copy_samples,
            throughput_mib_per_second: summarize_throughput(&throughput_samples),
        }],
    };
    write_json(&options.output, &result)?;
    println!(
        "Exported {} MiB {} times: first-progress p95 {:.3} ms, throughput p05 {:.1} MiB/s",
        EXPORT_SOURCE_BYTES / (1024 * 1024),
        options.iterations,
        result.scenarios[0].latency_ms.p95,
        result.scenarios[0].throughput_mib_per_second.p05,
    );
    Ok(())
}

fn measure_exports(
    options: &QueryOptions,
    workload: &Path,
    source: &Path,
) -> Result<ExportMeasurements, String> {
    let baseline_rss_bytes = current_rss_bytes();
    let sampler = MemorySampler::start(Duration::from_millis(2))?;
    let mut first_progress_samples = Vec::with_capacity(options.iterations);
    let mut copy_samples = Vec::with_capacity(options.iterations);
    let total_rounds = options.warmup + options.iterations;
    for round in 0..total_rounds {
        let destination = workload.join(format!("copy-{round:03}.epub"));
        let started = Instant::now();
        let mut first_progress = None;
        let outcome = export_file(source, &destination, OverwritePolicy::Deny, |_| {
            first_progress.get_or_insert_with(|| started.elapsed());
            ExportControl::Continue
        })
        .map_err(display_error)?;
        let elapsed = started.elapsed();
        if outcome.copied_bytes != EXPORT_SOURCE_BYTES {
            return Err(format!(
                "export copied {} bytes; expected {EXPORT_SOURCE_BYTES}",
                outcome.copied_bytes
            ));
        }
        if !files_equal(source, &destination)? {
            return Err(format!(
                "export bytes differ from source: {}",
                destination.display()
            ));
        }
        fs::remove_file(&destination).map_err(display_error)?;
        if round >= options.warmup {
            first_progress_samples.push(duration_ns(
                first_progress.ok_or_else(|| "export emitted no progress".to_owned())?,
            )?);
            copy_samples.push(duration_ns(elapsed)?);
        }
    }
    let peak_rss_bytes = sampler.finish()?;
    Ok(ExportMeasurements {
        first_progress_samples,
        copy_samples,
        baseline_rss_bytes,
        peak_rss_bytes,
    })
}

fn verify_export_failure_cases(workload: &Path, source: &Path) -> Result<(), String> {
    let collision = workload.join("collision.epub");
    fs::write(&collision, EXPORT_COLLISION_BYTES).map_err(display_error)?;
    let collision_preserved = matches!(
        export_file(source, &collision, OverwritePolicy::Deny, |_| {
            ExportControl::Continue
        }),
        Err(ExportError::DestinationExists(_))
    ) && fs::read(&collision).map_err(display_error)?
        == EXPORT_COLLISION_BYTES;
    let missing_source = workload.join("source-was-removed.epub");
    let missing_destination = workload.join("missing-source-copy.epub");
    let missing_source_rejected = matches!(
        export_file(
            &missing_source,
            &missing_destination,
            OverwritePolicy::Deny,
            |_| ExportControl::Continue,
        ),
        Err(ExportError::SourceUnavailable(_))
    ) && !missing_destination.exists();
    let temporary_cleanup_verified = temporary_exports(workload)? == 0;
    if !collision_preserved || !missing_source_rejected || !temporary_cleanup_verified {
        return Err("export correctness checks did not reconcile".into());
    }
    Ok(())
}

fn write_export_source(path: &Path) -> Result<(), String> {
    let mut block = vec![0_u8; EXPORT_SOURCE_BLOCK_BYTES];
    for (index, byte) in block.iter_mut().enumerate() {
        let index = u64::try_from(index).expect("source block index fits u64");
        *byte = splitmix64(DEFAULT_SEED ^ index).to_le_bytes()[0];
    }
    let file = File::create(path).map_err(display_error)?;
    let mut writer = BufWriter::with_capacity(EXPORT_SOURCE_BLOCK_BYTES, file);
    let blocks = EXPORT_SOURCE_BYTES
        / u64::try_from(EXPORT_SOURCE_BLOCK_BYTES).expect("source block size fits u64");
    for _ in 0..blocks {
        writer.write_all(&block).map_err(display_error)?;
    }
    writer.flush().map_err(display_error)
}

fn files_equal(first: &Path, second: &Path) -> Result<bool, String> {
    if fs::metadata(first).map_err(display_error)?.len()
        != fs::metadata(second).map_err(display_error)?.len()
    {
        return Ok(false);
    }
    let mut first = File::open(first).map_err(display_error)?;
    let mut second = File::open(second).map_err(display_error)?;
    let mut first_buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    let mut second_buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let first_read = first.read(&mut first_buffer).map_err(display_error)?;
        let second_read = second.read(&mut second_buffer).map_err(display_error)?;
        if first_read != second_read || first_buffer[..first_read] != second_buffer[..second_read] {
            return Ok(false);
        }
        if first_read == 0 {
            return Ok(true);
        }
    }
}

fn temporary_exports(directory: &Path) -> Result<usize, String> {
    fs::read_dir(directory)
        .map_err(display_error)?
        .try_fold(0_usize, |count, entry| {
            let entry = entry.map_err(display_error)?;
            Ok(count
                + usize::from(
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".lectern-export-"),
                ))
        })
}

fn summarize_throughput(samples: &[f64]) -> ThroughputSummary {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    ThroughputSummary {
        min: sorted[0],
        p05: sorted[nearest_rank_index(sorted.len(), 5)],
        mean: sorted.iter().sum::<f64>()
            / f64::from(u32::try_from(sorted.len()).expect("sample count fits u32")),
        p50: sorted[nearest_rank_index(sorted.len(), 50)],
        max: *sorted.last().expect("non-empty throughput samples"),
    }
}

fn nearest_rank_index(length: usize, percentile: usize) -> usize {
    (percentile * length)
        .div_ceil(100)
        .saturating_sub(1)
        .min(length - 1)
}

fn run_reimport(options: &QueryOptions) -> Result<(), String> {
    ensure_distinct_paths("database", &options.database, "output", &options.output)?;
    if !options.database.is_file() {
        return Err(format!(
            "benchmark database is not a file: {}",
            options.database.display()
        ));
    }

    let rounds = options
        .warmup
        .checked_add(options.iterations)
        .ok_or_else(|| "re-import iteration count overflowed".to_owned())?;
    let mut database = LibraryDatabase::open(&options.database).map_err(display_error)?;
    let library_books = database.count().map_err(display_error)?;
    if library_books == 0 {
        return Err(format!(
            "benchmark database contains no books: {}",
            options.database.display()
        ));
    }
    let candidates = prepare_reimport_candidates(&database, rounds)?;
    let mut samples_ns = Vec::with_capacity(options.iterations);

    for (round, candidate) in candidates.iter().enumerate() {
        let started = Instant::now();
        let ids = database
            .import_books(std::slice::from_ref(&candidate.incoming))
            .map_err(display_error)?;
        let elapsed = started.elapsed();
        if ids.as_slice() != [candidate.original.id] {
            return Err(format!(
                "known-path re-import did not retain book {}",
                candidate.original.id
            ));
        }
        validate_reimported_book(&database, candidate)?;
        if round >= options.warmup {
            samples_ns.push(duration_ns(elapsed)?);
        }
    }

    let final_library_books = database.count().map_err(display_error)?;
    if final_library_books != library_books {
        return Err(format!(
            "re-import benchmark changed book count: got {final_library_books}, expected {library_books}"
        ));
    }
    let result = ReimportResult {
        schema_version: 1,
        kind: "reimport",
        measured_at_unix_ms: unix_time_ms()?,
        database_path: options.database.display().to_string(),
        library_books,
        final_library_books,
        warmup_iterations: options.warmup,
        measured_iterations: options.iterations,
        metadata_preserved: true,
        assets_preserved: true,
        covers_preserved: true,
        scenarios: vec![ReimportScenarioResult {
            name: "reimport_known_path",
            successful_reimports: rounds,
            latency_ms: summarize_latency(&samples_ns),
            samples_ns,
        }],
    };
    write_json(&options.output, &result)?;
    println!(
        "Measured {} known-path re-imports over {} books",
        options.iterations, library_books
    );
    Ok(())
}

fn prepare_reimport_candidates(
    database: &LibraryDatabase,
    rounds: usize,
) -> Result<Vec<ReimportCandidate>, String> {
    let summaries = database
        .query(&LibraryQuery::default())
        .map_err(display_error)?
        .into_iter()
        .filter(|book| book.has_cover)
        .take(rounds)
        .collect::<Vec<_>>();
    if summaries.len() != rounds {
        return Err(format!(
            "re-import benchmark needs {rounds} covered books but found {}",
            summaries.len()
        ));
    }

    summaries
        .into_iter()
        .map(|summary| {
            let original = database
                .get_book(summary.id)
                .map_err(display_error)?
                .ok_or_else(|| format!("re-import candidate {} disappeared", summary.id))?;
            let original_cover = database
                .load_cover(summary.id)
                .map_err(display_error)?
                .ok_or_else(|| format!("re-import candidate {} lost its cover", summary.id))?;
            let incoming = BookImport {
                book: BookMetadataDraft {
                    title: original.title.clone(),
                    authors: original.authors.clone(),
                    series: original.series.clone(),
                    publisher: original.publisher.clone(),
                    language: original.language.clone(),
                    description: original.description.clone(),
                },
                assets: original
                    .assets
                    .iter()
                    .map(|asset| BookAssetDraft {
                        format: asset.format,
                        storage: asset.storage,
                        path: asset.path.clone(),
                    })
                    .collect(),
                cover_thumbnail: None,
            };
            Ok(ReimportCandidate {
                original,
                original_cover,
                incoming,
            })
        })
        .collect()
}

fn validate_reimported_book(
    database: &LibraryDatabase,
    candidate: &ReimportCandidate,
) -> Result<(), String> {
    let updated = database
        .get_book(candidate.original.id)
        .map_err(display_error)?
        .ok_or_else(|| format!("re-imported book {} disappeared", candidate.original.id))?;
    if !book_metadata_matches(&candidate.original, &updated) {
        return Err(format!(
            "re-import changed metadata for book {}",
            candidate.original.id
        ));
    }
    if updated.assets != candidate.original.assets {
        return Err(format!(
            "re-import changed assets for book {}",
            candidate.original.id
        ));
    }
    if database
        .load_cover(candidate.original.id)
        .map_err(display_error)?
        .as_deref()
        != Some(candidate.original_cover.as_slice())
    {
        return Err(format!(
            "re-import changed the cover for book {}",
            candidate.original.id
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct FileFingerprint {
    bytes: u64,
    hash: u64,
}

fn fingerprint_file(path: &Path) -> Result<FileFingerprint, String> {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut file = File::open(path).map_err(display_error)?;
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut bytes = 0_u64;
    let mut hash = FNV_OFFSET;
    loop {
        let read = file.read(&mut buffer).map_err(display_error)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(u64::try_from(read).expect("buffer length fits u64"))
            .ok_or_else(|| format!("file size overflowed for {}", path.display()))?;
        for byte in &buffer[..read] {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    }
    Ok(FileFingerprint { bytes, hash })
}

#[derive(Serialize)]
struct ImportResult {
    schema_version: u32,
    kind: &'static str,
    measured_at_unix_ms: u128,
    database_path: String,
    corpus_path: String,
    corpus: CorpusStats,
    timing: ImportTiming,
    memory: ImportMemory,
    progress: Vec<ImportProgressSample>,
    summary: ImportOutcome,
    database_books: u64,
    database_bytes: u64,
}

#[derive(Serialize)]
struct CorpusStats {
    files: usize,
    epub_files: usize,
    pdf_files: usize,
    total_bytes: u64,
    file_size_bytes: SizeSummary,
    post_import_inspection_ms: f64,
}

#[derive(Serialize)]
struct SizeSummary {
    min: u64,
    p50: u64,
    p95: u64,
    max: u64,
}

#[derive(Serialize)]
struct ImportTiming {
    discovery_ms: Option<f64>,
    total_ms: f64,
    imported_files_per_second: f64,
}

#[derive(Serialize)]
struct ImportMemory {
    definition: &'static str,
    baseline_rss_bytes: Option<u64>,
    peak_rss_bytes: Option<u64>,
    peak_delta_bytes: Option<u64>,
    sample_interval_ms: u64,
}

#[derive(Serialize)]
struct ImportProgressSample {
    elapsed_ms: f64,
    discovered: usize,
    processed: usize,
    imported: usize,
    failed: usize,
}

#[derive(Serialize)]
struct ImportOutcome {
    discovered: usize,
    imported: usize,
    failed: usize,
    failures: Vec<ImportFailureResult>,
    failures_omitted: usize,
}

#[derive(Serialize)]
struct ImportFailureResult {
    path: String,
    message: String,
}

struct ImportMeasurements {
    elapsed: Duration,
    baseline_rss_bytes: Option<u64>,
    peak_rss_bytes: Option<u64>,
    progress: Vec<ImportProgressSample>,
    summary: ImportSummary,
}

fn run_import(options: &ImportOptions) -> Result<(), String> {
    ensure_distinct_paths("database", &options.database, "output", &options.output)?;
    ensure_distinct_paths("database", &options.database, "corpus", &options.corpus)?;
    ensure_distinct_paths("output", &options.output, "corpus", &options.corpus)?;
    ensure_outside_directory("database", &options.database, "corpus", &options.corpus)?;
    ensure_outside_directory("output", &options.output, "corpus", &options.corpus)?;
    if !options.corpus.exists() {
        return Err(format!(
            "corpus does not exist: {}",
            options.corpus.display()
        ));
    }
    if options.database.exists() && !options.replace {
        return Err(format!(
            "{} already exists; choose a new benchmark path or pass --replace",
            options.database.display()
        ));
    }
    if options.database.exists() {
        let publications =
            discover_publications(std::slice::from_ref(&options.corpus)).map_err(display_error)?;
        if publications.is_empty() {
            return Err(format!(
                "no EPUB or PDF files found beneath {}; existing database was preserved",
                options.corpus.display()
            ));
        }
    }
    remove_database_files(&options.database)?;

    let baseline_rss_bytes = current_rss_bytes();
    let sampler = MemorySampler::start(Duration::from_millis(20))?;
    let started = Instant::now();
    let mut progress = Vec::new();
    let result = import_paths(
        &options.database,
        std::slice::from_ref(&options.corpus),
        |sample| progress.push(progress_sample(started.elapsed(), sample)),
    );
    let elapsed = started.elapsed();
    let peak_rss_bytes = sampler.finish()?;
    let summary = result.map_err(display_error)?;
    let publications =
        discover_publications(std::slice::from_ref(&options.corpus)).map_err(display_error)?;
    if publications.is_empty() {
        return Err(format!(
            "no EPUB or PDF files found beneath {}",
            options.corpus.display()
        ));
    }
    let corpus = inspect_corpus(&publications)?;
    let measurements = ImportMeasurements {
        elapsed,
        baseline_rss_bytes,
        peak_rss_bytes,
        progress,
        summary,
    };
    let output = assemble_import_result(options, corpus, measurements)?;
    write_json(&options.output, &output)?;
    println!(
        "Imported {} of {} files in {:.1} ms ({:.1} files/s)",
        output.summary.imported,
        output.summary.discovered,
        output.timing.total_ms,
        output.timing.imported_files_per_second
    );
    Ok(())
}

fn assemble_import_result(
    options: &ImportOptions,
    corpus: CorpusStats,
    measurements: ImportMeasurements,
) -> Result<ImportResult, String> {
    let database = LibraryDatabase::open(&options.database).map_err(display_error)?;
    let database_books = database.count().map_err(display_error)?;
    drop(database);

    let failure_count = measurements.summary.failures.len();
    let failures = measurements
        .summary
        .failures
        .iter()
        .take(MAX_RECORDED_FAILURES)
        .map(|failure| ImportFailureResult {
            path: failure.path.display().to_string(),
            message: failure.message.clone(),
        })
        .collect::<Vec<_>>();
    let discovery_ms = measurements
        .progress
        .first()
        .map(|sample| sample.elapsed_ms);
    let imported_files_per_second = if measurements.elapsed.is_zero() {
        0.0
    } else {
        Duration::from_secs(
            u64::try_from(measurements.summary.imported).expect("import count fits u64"),
        )
        .as_secs_f64()
            / measurements.elapsed.as_secs_f64()
    };
    Ok(ImportResult {
        schema_version: 1,
        kind: "import",
        measured_at_unix_ms: unix_time_ms()?,
        database_path: options.database.display().to_string(),
        corpus_path: options.corpus.display().to_string(),
        corpus,
        timing: ImportTiming {
            discovery_ms,
            total_ms: duration_ms(measurements.elapsed),
            imported_files_per_second,
        },
        memory: ImportMemory {
            definition: "Linux process resident set size sampled from /proc/self/status",
            baseline_rss_bytes: measurements.baseline_rss_bytes,
            peak_rss_bytes: measurements.peak_rss_bytes,
            peak_delta_bytes: measurements
                .peak_rss_bytes
                .zip(measurements.baseline_rss_bytes)
                .map(|(peak, baseline)| peak.saturating_sub(baseline)),
            sample_interval_ms: 20,
        },
        progress: measurements.progress,
        summary: ImportOutcome {
            discovered: measurements.summary.discovered,
            imported: measurements.summary.imported,
            failed: measurements.summary.failed,
            failures,
            failures_omitted: failure_count.saturating_sub(MAX_RECORDED_FAILURES),
        },
        database_books,
        database_bytes: fs::metadata(&options.database)
            .map_err(display_error)?
            .len(),
    })
}

fn inspect_corpus(paths: &[PathBuf]) -> Result<CorpusStats, String> {
    let started = Instant::now();
    let mut epub_files = 0;
    let mut pdf_files = 0;
    let mut sizes = Vec::with_capacity(paths.len());
    for path in paths {
        match path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("epub") => epub_files += 1,
            Some("pdf") => pdf_files += 1,
            _ => {}
        }
        sizes.push(fs::metadata(path).map_err(display_error)?.len());
    }
    sizes.sort_unstable();
    let total_bytes = sizes.iter().try_fold(0_u64, |total, size| {
        total
            .checked_add(*size)
            .ok_or_else(|| "corpus size exceeds u64".to_owned())
    })?;
    Ok(CorpusStats {
        files: paths.len(),
        epub_files,
        pdf_files,
        total_bytes,
        file_size_bytes: SizeSummary {
            min: sizes[0],
            p50: nearest_rank(&sizes, 50),
            p95: nearest_rank(&sizes, 95),
            max: *sizes.last().expect("non-empty corpus"),
        },
        post_import_inspection_ms: duration_ms(started.elapsed()),
    })
}

fn progress_sample(elapsed: Duration, progress: ImportProgress) -> ImportProgressSample {
    ImportProgressSample {
        elapsed_ms: duration_ms(elapsed),
        discovered: progress.discovered,
        processed: progress.processed,
        imported: progress.imported,
        failed: progress.failed,
    }
}

struct MemorySampler {
    peak_bytes: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl MemorySampler {
    fn start(interval: Duration) -> Result<Self, String> {
        let peak_bytes = Arc::new(AtomicU64::new(current_rss_bytes().unwrap_or(0)));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_peak = Arc::clone(&peak_bytes);
        let thread_stop = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("lectern-benchmark-memory".into())
            .spawn(move || {
                while !thread_stop.load(Ordering::Relaxed) {
                    if let Some(rss) = current_rss_bytes() {
                        thread_peak.fetch_max(rss, Ordering::Relaxed);
                    }
                    thread::sleep(interval);
                }
            })
            .map_err(display_error)?;
        Ok(Self {
            peak_bytes,
            stop,
            thread: Some(thread),
        })
    }

    fn finish(mut self) -> Result<Option<u64>, String> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| "memory sampler thread panicked".to_owned())?;
        }
        if let Some(rss) = current_rss_bytes() {
            self.peak_bytes.fetch_max(rss, Ordering::Relaxed);
        }
        let peak = self.peak_bytes.load(Ordering::Relaxed);
        Ok((peak > 0).then_some(peak))
    }
}

fn current_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    parse_linux_rss_bytes(&status)
}

fn parse_linux_rss_bytes(status: &str) -> Option<u64> {
    let kilobytes = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    kilobytes.checked_mul(1_024)
}

fn query_scenarios() -> Vec<QueryScenario> {
    vec![
        QueryScenario {
            name: "search_title_prefix",
            search: "Amber",
            format: None,
            asset_health: None,
            sort: SortOrder::Title,
        },
        QueryScenario {
            name: "search_author_prefix",
            search: "Author 0042",
            format: None,
            asset_health: None,
            sort: SortOrder::Title,
        },
        QueryScenario {
            name: "filter_epub",
            search: "",
            format: Some(BookFormat::Epub),
            asset_health: None,
            sort: SortOrder::Title,
        },
        QueryScenario {
            name: "sort_title",
            search: "",
            format: None,
            asset_health: None,
            sort: SortOrder::Title,
        },
        QueryScenario {
            name: "sort_author",
            search: "",
            format: None,
            asset_health: None,
            sort: SortOrder::Author,
        },
        QueryScenario {
            name: "sort_recently_added",
            search: "",
            format: None,
            asset_health: None,
            sort: SortOrder::RecentlyAdded,
        },
        QueryScenario {
            name: "search_filter_sort",
            search: "Luminous",
            format: Some(BookFormat::Pdf),
            asset_health: None,
            sort: SortOrder::Author,
        },
        QueryScenario {
            name: "filter_unchecked_assets",
            search: "",
            format: None,
            asset_health: Some(AssetHealth::Unknown),
            sort: SortOrder::Title,
        },
    ]
}

fn page_query_scenarios() -> Vec<PageQueryScenario> {
    vec![
        PageQueryScenario {
            name: "first_page_title",
            search: "",
            format: None,
            asset_health: None,
            sort: SortOrder::Title,
            position: PagePosition::First,
        },
        PageQueryScenario {
            name: "deep_page_title",
            search: "",
            format: None,
            asset_health: None,
            sort: SortOrder::Title,
            position: PagePosition::Deep,
        },
        PageQueryScenario {
            name: "first_page_search_filter",
            search: "Luminous",
            format: Some(BookFormat::Pdf),
            asset_health: None,
            sort: SortOrder::Author,
            position: PagePosition::First,
        },
    ]
}

fn covered_page_query_scenarios() -> Vec<PageQueryScenario> {
    vec![
        PageQueryScenario {
            name: "first_page_title",
            search: "",
            format: None,
            asset_health: None,
            sort: SortOrder::Title,
            position: PagePosition::First,
        },
        PageQueryScenario {
            name: "first_page_author",
            search: "",
            format: None,
            asset_health: None,
            sort: SortOrder::Author,
            position: PagePosition::First,
        },
        PageQueryScenario {
            name: "first_page_recently_added",
            search: "",
            format: None,
            asset_health: None,
            sort: SortOrder::RecentlyAdded,
            position: PagePosition::First,
        },
        PageQueryScenario {
            name: "deep_page_title",
            search: "",
            format: None,
            asset_health: None,
            sort: SortOrder::Title,
            position: PagePosition::Deep,
        },
        PageQueryScenario {
            name: "first_page_search_filter",
            search: "Luminous",
            format: Some(BookFormat::Pdf),
            asset_health: None,
            sort: SortOrder::Author,
            position: PagePosition::First,
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
    let mut temporary = path.as_os_str().to_os_string();
    temporary.push(format!(".tmp-{}", process::id()));
    let temporary = PathBuf::from(temporary);
    let result = (|| {
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(display_error)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer_pretty(&mut writer, value).map_err(display_error)?;
        writer.write_all(b"\n").map_err(display_error)?;
        writer.flush().map_err(display_error)?;
        drop(writer);
        fs::rename(&temporary, path).map_err(display_error)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, path::Path};

    use super::{
        Arguments, BENCHMARK_COVER_HEIGHT, BENCHMARK_COVER_WIDTH, ImportOptions, QueryOptions,
        SeedOptions, benchmark_record, ensure_distinct_paths, make_benchmark_cover, nearest_rank,
        parse_linux_rss_bytes, splitmix64,
    };

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
    fn parses_import_options() {
        let options = ImportOptions::parse(&mut arguments(&[
            "--database",
            "import.sqlite3",
            "--corpus",
            "corpus",
            "--output",
            "import.json",
            "--replace",
        ]))
        .expect("parse import options");

        assert_eq!(options.corpus.to_string_lossy(), "corpus");
        assert!(options.replace);
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
    fn benchmark_cover_has_representative_dimensions() {
        let encoded = make_benchmark_cover().expect("encode benchmark cover");
        let image = image::load_from_memory(&encoded).expect("decode benchmark cover");

        assert_eq!(image.width(), BENCHMARK_COVER_WIDTH);
        assert_eq!(image.height(), BENCHMARK_COVER_HEIGHT);
        assert!(encoded.len() > 10_000);
    }

    #[test]
    fn equivalent_paths_are_not_accepted_as_distinct() {
        let result = ensure_distinct_paths(
            "database",
            Path::new("target/../benchmark.json"),
            "output",
            Path::new("benchmark.json"),
        );

        assert!(result.is_err());
    }

    #[test]
    fn nearest_rank_uses_observed_samples() {
        let samples = (1..=100).collect::<Vec<_>>();

        assert_eq!(nearest_rank(&samples, 50), 50);
        assert_eq!(nearest_rank(&samples, 95), 95);
        assert_eq!(nearest_rank(&samples, 99), 99);
    }

    #[test]
    fn parses_linux_resident_memory() {
        let status = "Name:\tlectern\nVmSize:\t  9000 kB\nVmRSS:\t  1234 kB\nThreads:\t4\n";

        assert_eq!(parse_linux_rss_bytes(status), Some(1_263_616));
        assert_eq!(parse_linux_rss_bytes("Name:\tlectern\n"), None);
    }
}
