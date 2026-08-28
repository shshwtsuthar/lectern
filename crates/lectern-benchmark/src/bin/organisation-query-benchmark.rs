//! Release-mode normalized query and autocomplete regression workload.

use std::{
    collections::HashSet,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use lectern_core::{
    BookFormat, BookId, LibraryQuery, SortOrder,
    organisation::{
        BookEdit, BookIdentifierEdit, ContributorCreditEdit, ContributorFacet, ContributorId,
        ContributorReference, ExactFacets, IdentifierTypeReference, SeriesId, SeriesIndex,
        SeriesMembershipEdit, SeriesReference, TagId, TagReference,
    },
};
use lectern_storage::LibraryDatabase;
use rusqlite::{Connection, TransactionBehavior, params};
use serde::Serialize;

const USAGE: &str = "Usage:
  organisation-query-benchmark seed --database PATH --output PATH [OPTIONS]
  organisation-query-benchmark query --database PATH --output PATH [OPTIONS]

Options:
  --books N          Logical books in the fixture (default: 50000)
  --seed N           Deterministic fixture seed (default: 20260825)
  --cover-every N    Give every Nth book a cover; zero disables covers (default: 3)
  --fixture-version N Normalized fixture version, 2 or 3 (default: 2)
  --iterations N     Measured iterations per scenario (default: 40)
  --warmup N         Warmup iterations per scenario (default: 10)
";

const CONTRIBUTORS: u64 = 20_000;
const SERIES: u64 = 2_500;
const TAGS: u64 = 500;
const TAGS_PER_BOOK: u64 = 8;
const SAVED_SEARCHES: u64 = 250;
const PAGE_SIZE: u32 = 128;
const AUTOCOMPLETE_LIMIT: u32 = 50;
const IDENTIFIERS_PER_BOOK: u64 = 3;

#[derive(Clone, Debug)]
struct Options {
    database: PathBuf,
    output: PathBuf,
    books: u64,
    seed: u64,
    cover_every: u64,
    fixture_version: u32,
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
            fixture_version: 2,
            iterations: 40,
            warmup: 10,
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
                "--fixture-version" => options.fixture_version = parse_number(&name, value)?,
                "--iterations" => options.iterations = parse_number(&name, value)?,
                "--warmup" => options.warmup = parse_number(&name, value)?,
                _ => return Err(format!("unknown option {name:?}")),
            }
        }
        if !matches!(command.as_str(), "seed" | "query") {
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
        if !matches!(options.fixture_version, 2 | 3) {
            return Err("--fixture-version must be 2 or 3".into());
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
        if command == "seed" {
            seed(&options)
        } else {
            run_queries(&options)
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
struct SeedResult {
    schema_version: u32,
    fixture_version: u32,
    kind: &'static str,
    database_path: String,
    library_books: u64,
    contributors: u64,
    series: u64,
    tags: u64,
    tags_per_book: u64,
    identifiers_per_book: u64,
    saved_searches: u64,
    seed: u64,
    database_bytes: u64,
    elapsed_ms: f64,
}

#[allow(clippy::too_many_lines)]
fn seed(options: &Options) -> Result<(), String> {
    ensure_new_file(&options.database)?;
    ensure_new_file(&options.output)?;
    create_parent(&options.database)?;
    create_parent(&options.output)?;
    let started = Instant::now();

    drop(LibraryDatabase::open(&options.database).map_err(display_error)?);
    let mut connection = Connection::open(&options.database).map_err(display_error)?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(display_error)?;
    connection
        .pragma_update(None, "synchronous", "OFF")
        .map_err(display_error)?;
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(display_error)?;
    transaction
        .pragma_update(None, "defer_foreign_keys", "ON")
        .map_err(display_error)?;

    {
        let mut insert = transaction
            .prepare(
                "INSERT INTO contributors(id, display_name, sort_name, identity_key, sort_key) \
                 VALUES (?1, ?2, ?2, ?3, ?3)",
            )
            .map_err(display_error)?;
        for id in 1..=CONTRIBUTORS.min(options.books) {
            let name = format!("Contributor {id:05}");
            insert
                .execute(params![to_i64(id)?, name, name.to_ascii_lowercase()])
                .map_err(display_error)?;
        }
    }
    {
        let mut insert = transaction
            .prepare("INSERT INTO series_entities(id, name, identity_key) VALUES (?1, ?2, ?3)")
            .map_err(display_error)?;
        for id in 1..=SERIES.min(options.books) {
            let name = format!("Series {id:04}");
            insert
                .execute(params![to_i64(id)?, name, name.to_ascii_lowercase()])
                .map_err(display_error)?;
        }
    }
    {
        let mut insert = transaction
            .prepare("INSERT INTO tags(id, name, identity_key) VALUES (?1, ?2, ?3)")
            .map_err(display_error)?;
        for id in 1..=TAGS.min(options.books) {
            let name = format!("Tag {id:03}");
            insert
                .execute(params![to_i64(id)?, name, name.to_ascii_lowercase()])
                .map_err(display_error)?;
        }
    }

    let mut insert_book = transaction
        .prepare(
            "INSERT INTO books( \
                 id, title, sort_title, authors, sort_authors, series, publisher, language, \
                 description, has_cover, has_file_issue, added_at, modified_at, authors_search, \
                 contributors_search, tags_search, series_key, series_index \
             ) VALUES ( \
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12, ?4, ?13, ?14, \
                 ?15, ?16 \
             )",
        )
        .map_err(display_error)?;
    let mut insert_asset = transaction
        .prepare(
            "INSERT INTO book_assets( \
                 id, book_id, format, storage_mode, health, path_encoding, path \
             ) VALUES (?1, ?1, ?2, 'reference', ?3, 'utf8', ?4)",
        )
        .map_err(display_error)?;
    let mut insert_credit = transaction
        .prepare(
            "INSERT INTO book_contributors( \
                 book_id, contributor_id, role, position, \
                 display_name_projection, sort_key_projection \
             ) VALUES (?1, ?2, ?3, 0, ?4, ?5)",
        )
        .map_err(display_error)?;
    let mut insert_membership = transaction
        .prepare(
            "INSERT INTO series_memberships( \
                 book_id, series_id, series_index, name_projection, key_projection \
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(display_error)?;
    let mut insert_tag = transaction
        .prepare("INSERT INTO book_tags(book_id, tag_id) VALUES (?1, ?2)")
        .map_err(display_error)?;
    let mut insert_cover = transaction
        .prepare("INSERT INTO book_covers(book_id, jpeg) VALUES (?1, ?2)")
        .map_err(display_error)?;
    let mut insert_identifier = (options.fixture_version >= 3)
        .then(|| {
            transaction.prepare(
                "INSERT INTO book_identifiers(book_id, identifier_type_id, value) \
                 VALUES (?1, ?2, ?3)",
            )
        })
        .transpose()
        .map_err(display_error)?;
    let cover = representative_cover();

    for offset in 0..options.books {
        let id = offset + 1;
        let id_i64 = to_i64(id)?;
        let title = format!("Title {offset:05}");
        let credit_count = 1 + mixed_index(offset, options.seed, 4);
        let mut contributor_names = Vec::with_capacity(usize::try_from(credit_count).unwrap_or(4));
        for credit in 0..credit_count {
            let contributor_id = 1 + mixed_index(
                offset.wrapping_add(credit.wrapping_mul(4_999)),
                options.seed,
                CONTRIBUTORS.min(options.books),
            );
            let name = format!("Contributor {contributor_id:05}");
            let role = ["author", "editor", "translator", "illustrator"]
                [usize::try_from(credit).map_err(display_error)?];
            insert_credit
                .execute(params![
                    id_i64,
                    to_i64(contributor_id)?,
                    role,
                    name,
                    name.to_ascii_lowercase(),
                ])
                .map_err(display_error)?;
            contributor_names.push(name);
        }
        let authors = contributor_names[0].clone();
        let contributors_search = contributor_names.join(" ");

        let membership = (offset % 10 < 7)
            .then(|| 1 + mixed_index(offset, options.seed ^ 0x51_7e, SERIES.min(options.books)));
        let (series_name, series_key, series_index) = if let Some(series_id) = membership {
            let name = format!("Series {series_id:04}");
            let key = name.to_ascii_lowercase();
            // Fixture v2 keeps the familiar 1–50 whole-number order and uses millionths as a
            // deterministic tie-breaker, satisfying series-local number uniqueness.
            let index = i64::try_from((offset % 50 + 1) * 1_000_000 + (offset / 50))
                .map_err(display_error)?;
            insert_membership
                .execute(params![id_i64, to_i64(series_id)?, index, name, key])
                .map_err(display_error)?;
            (Some(name), Some(key), Some(index))
        } else {
            (None, None, None)
        };

        let tag_base = offset % TAGS.min(options.books);
        let mut tag_names = Vec::with_capacity(usize::try_from(TAGS_PER_BOOK).unwrap_or(8));
        for tag_offset in 0..TAGS_PER_BOOK.min(options.books) {
            let tag_id = 1 + (tag_base + tag_offset) % TAGS.min(options.books);
            insert_tag
                .execute(params![id_i64, to_i64(tag_id)?])
                .map_err(display_error)?;
            tag_names.push(format!("Tag {tag_id:03}"));
        }
        let format = if id % 2 == 1 { "epub" } else { "pdf" };
        let health = match id % 20 {
            0 => "missing",
            1 => "unreadable",
            2..=8 => "available",
            _ => "unknown",
        };
        let timestamp = 1_700_000_000_i64 + i64::try_from(offset).map_err(display_error)?;
        let has_cover = options.cover_every != 0 && offset % options.cover_every == 0;
        insert_book
            .execute(params![
                id_i64,
                title,
                title.to_ascii_lowercase(),
                authors,
                authors.to_ascii_lowercase(),
                series_name,
                format!("Publisher {:03}", offset % 200),
                if offset % 5 == 0 { "fr" } else { "en" },
                format!("Deterministic organisation fixture {offset}"),
                has_cover,
                matches!(health, "missing" | "unreadable"),
                timestamp,
                contributors_search,
                tag_names.join(" "),
                series_key,
                series_index,
            ])
            .map_err(display_error)?;
        insert_asset
            .execute(params![
                id_i64,
                format,
                health,
                format!("/benchmark/organisation/{id}.{format}").into_bytes(),
            ])
            .map_err(display_error)?;
        if has_cover {
            insert_cover
                .execute(params![id_i64, cover])
                .map_err(display_error)?;
        }
        if let Some(insert_identifier) = &mut insert_identifier {
            for (identifier_type, value) in [
                (1_i64, format!("978-0-{id:010}")),
                (1_i64, format!("978-1-{id:010}")),
                (3_i64, format!("10.0000/lectern.{id}")),
            ] {
                insert_identifier
                    .execute(params![id_i64, identifier_type, value])
                    .map_err(display_error)?;
            }
        }
    }

    {
        let mut insert = transaction
            .prepare(
                "INSERT INTO saved_searches(id, name, identity_key, search_expression) \
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .map_err(display_error)?;
        for id in 1..=SAVED_SEARCHES {
            let name = format!("Saved search {id:03}");
            insert
                .execute(params![
                    to_i64(id)?,
                    name,
                    name.to_ascii_lowercase(),
                    format!("title:title tag:\"Tag {:03}\"", 1 + id % TAGS),
                ])
                .map_err(display_error)?;
        }
    }
    drop(insert_identifier);
    drop(insert_cover);
    drop(insert_tag);
    drop(insert_membership);
    drop(insert_credit);
    drop(insert_asset);
    drop(insert_book);
    transaction.commit().map_err(display_error)?;
    connection
        .execute("INSERT INTO books_fts(books_fts) VALUES ('optimize')", [])
        .map_err(display_error)?;
    drop(connection);

    let result = SeedResult {
        schema_version: 1,
        fixture_version: options.fixture_version,
        kind: "organisation-query-seed",
        database_path: options.database.display().to_string(),
        library_books: options.books,
        contributors: CONTRIBUTORS.min(options.books),
        series: SERIES.min(options.books),
        tags: TAGS.min(options.books),
        tags_per_book: TAGS_PER_BOOK.min(options.books),
        identifiers_per_book: if options.fixture_version >= 3 {
            IDENTIFIERS_PER_BOOK
        } else {
            0
        },
        saved_searches: SAVED_SEARCHES,
        seed: options.seed,
        database_bytes: fs::metadata(&options.database)
            .map_err(display_error)?
            .len(),
        elapsed_ms: started.elapsed().as_secs_f64() * 1_000.0,
    };
    write_json(&options.output, &result)?;
    println!(
        "Seeded normalized query library with {} books in {:.1} ms",
        options.books, result.elapsed_ms
    );
    Ok(())
}

#[derive(Clone, Copy)]
enum Scenario {
    Contributor,
    Series,
    IncludedTags,
    IncludedExcludedTags,
    Combined,
    DeepPage,
    Autocomplete,
    SeriesIndexAvailability,
    IdentifierMetadataRoundTrip,
}

impl Scenario {
    const ALL: [Self; 9] = [
        Self::Contributor,
        Self::Series,
        Self::IncludedTags,
        Self::IncludedExcludedTags,
        Self::Combined,
        Self::DeepPage,
        Self::Autocomplete,
        Self::SeriesIndexAvailability,
        Self::IdentifierMetadataRoundTrip,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::Contributor => "contributor_first_page",
            Self::Series => "series_first_page_sorted",
            Self::IncludedTags => "two_included_tags_first_page",
            Self::IncludedExcludedTags => "included_excluded_tags_first_page",
            Self::Combined => "combined_fielded_projection_first_page",
            Self::DeepPage => "deep_bounded_page_without_count",
            Self::Autocomplete => "bounded_vocabulary_autocomplete",
            Self::SeriesIndexAvailability => "series_index_availability",
            Self::IdentifierMetadataRoundTrip => "identifier_metadata_round_trip",
        }
    }
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
    page_size: u32,
    autocomplete_limit: u32,
    verified_checks: Vec<&'static str>,
    query_plans: Vec<QueryPlan>,
    scenarios: Vec<QueryScenario>,
}

#[derive(Serialize)]
struct QueryPlan {
    name: &'static str,
    required_index: &'static str,
    details: Vec<String>,
}

#[derive(Serialize)]
struct QueryScenario {
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

#[derive(Default)]
struct Observation {
    ids: Vec<i64>,
    total: Option<u64>,
}

fn run_queries(options: &Options) -> Result<(), String> {
    if !options.database.is_file() {
        return Err(format!(
            "query database is not a file: {}",
            options.database.display()
        ));
    }
    ensure_new_file(&options.output)?;
    create_parent(&options.output)?;
    validate_seed(&options.database, options.books)?;
    let mut database = LibraryDatabase::open(&options.database).map_err(display_error)?;
    let rounds = options
        .warmup
        .checked_add(options.iterations)
        .ok_or_else(|| "query iteration count overflowed".to_owned())?;
    let mut scenarios = Vec::with_capacity(Scenario::ALL.len());

    for scenario in Scenario::ALL {
        let mut samples = Vec::with_capacity(options.iterations);
        let mut observed_results = 0;
        for round in 0..rounds {
            let started = Instant::now();
            let observation = run_scenario(&mut database, scenario, options.seed, options.books)?;
            let elapsed = started.elapsed();
            validate_observation(scenario, &observation, options.books)?;
            observed_results = observation.ids.len();
            if round >= options.warmup {
                samples.push(duration_ns(elapsed)?);
            }
        }
        scenarios.push(QueryScenario {
            name: scenario.name(),
            successful_operations: rounds,
            observed_results,
            latency_ms: summarize_latency(&samples),
            samples_ns: samples,
        });
    }
    drop(database);

    let connection = Connection::open(&options.database).map_err(display_error)?;
    let query_plans = collect_query_plans(&connection)?;
    let result = QueryResult {
        schema_version: 1,
        kind: "organisation-query",
        measured_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(display_error)?
            .as_millis(),
        database_path: options.database.display().to_string(),
        library_books: options.books,
        warmup_iterations: options.warmup,
        measured_iterations: options.iterations,
        page_size: PAGE_SIZE,
        autocomplete_limit: AUTOCOMPLETE_LIMIT,
        verified_checks: vec![
            "exact_ids",
            "matching_counts",
            "stable_order",
            "unique_book_rows",
            "covering_query_plans",
            "bounded_autocomplete",
            "series_index_availability",
            "identifier_metadata_round_trip",
        ],
        query_plans,
        scenarios,
    };
    write_json(&options.output, &result)?;
    println!(
        "Measured {} normalized query scenarios over {} books",
        Scenario::ALL.len(),
        options.books
    );
    Ok(())
}

fn run_scenario(
    database: &mut LibraryDatabase,
    scenario: Scenario,
    seed: u64,
    books: u64,
) -> Result<Observation, String> {
    let contributor_id = 1 + seed % CONTRIBUTORS.min(books);
    let series_id = 1 + (seed ^ 0x51_7e) % SERIES.min(books);
    match scenario {
        Scenario::Contributor => first_page(
            database,
            &LibraryQuery {
                facets: ExactFacets::new(
                    vec![ContributorFacet {
                        contributor: ContributorId::new(to_i64(contributor_id)?),
                        author_only: false,
                    }],
                    None,
                    Vec::new(),
                    Vec::new(),
                )
                .map_err(display_error)?,
                ..LibraryQuery::default()
            },
        ),
        Scenario::Series => first_page(
            database,
            &LibraryQuery {
                facets: ExactFacets::new(
                    Vec::new(),
                    Some(SeriesId::new(to_i64(series_id)?)),
                    Vec::new(),
                    Vec::new(),
                )
                .map_err(display_error)?,
                sort: SortOrder::Series,
                ..LibraryQuery::default()
            },
        ),
        Scenario::IncludedTags => first_page(
            database,
            &LibraryQuery {
                facets: ExactFacets::new(
                    Vec::new(),
                    None,
                    vec![TagId::new(1), TagId::new(2)],
                    Vec::new(),
                )
                .map_err(display_error)?,
                ..LibraryQuery::default()
            },
        ),
        Scenario::IncludedExcludedTags => first_page(
            database,
            &LibraryQuery {
                facets: ExactFacets::new(
                    Vec::new(),
                    None,
                    vec![TagId::new(1)],
                    vec![TagId::new(9)],
                )
                .map_err(display_error)?,
                ..LibraryQuery::default()
            },
        ),
        Scenario::Combined => first_page(
            database,
            &LibraryQuery {
                search: format!(
                    "author:\"Contributor {contributor_id:05}\" tag:\"Tag 001\" format:epub"
                ),
                facets: ExactFacets::new(
                    vec![ContributorFacet {
                        contributor: ContributorId::new(to_i64(contributor_id)?),
                        author_only: true,
                    }],
                    None,
                    vec![TagId::new(1)],
                    Vec::new(),
                )
                .map_err(display_error)?,
                format: Some(BookFormat::Epub),
                sort: SortOrder::Author,
                ..LibraryQuery::default()
            },
        ),
        Scenario::DeepPage => database
            .query_window(&LibraryQuery::default(), 4_096, PAGE_SIZE)
            .map(|books| Observation {
                ids: books.into_iter().map(|book| book.id.value()).collect(),
                total: None,
            })
            .map_err(display_error),
        Scenario::Autocomplete => run_autocomplete(database),
        Scenario::SeriesIndexAvailability => run_series_index_availability(database, seed, books),
        Scenario::IdentifierMetadataRoundTrip => run_identifier_metadata_round_trip(database),
    }
}

fn first_page(database: &mut LibraryDatabase, query: &LibraryQuery) -> Result<Observation, String> {
    database
        .query_page(query, 0, PAGE_SIZE)
        .map(|page| Observation {
            ids: page.books.into_iter().map(|book| book.id.value()).collect(),
            total: Some(page.total),
        })
        .map_err(display_error)
}

fn run_autocomplete(database: &LibraryDatabase) -> Result<Observation, String> {
    let contributors = database
        .autocomplete_contributors("contributor 00", &[ContributorId::new(1)], 50)
        .map_err(display_error)?;
    let series = database
        .autocomplete_series("series 00", &[SeriesId::new(1)], 50)
        .map_err(display_error)?;
    let tags = database
        .autocomplete_tags("tag 0", &[TagId::new(1)], 50)
        .map_err(display_error)?;
    let identifier_types = database
        .autocomplete_identifier_types("is", 50)
        .map_err(display_error)?;
    let mut ids = contributors
        .into_iter()
        .map(|entry| entry.contributor.id.value())
        .collect::<Vec<_>>();
    ids.extend(series.into_iter().map(|entry| -entry.series.id.value()));
    ids.extend(
        tags.into_iter()
            .map(|entry| -100_000 - entry.tag.id.value()),
    );
    ids.extend(
        identifier_types
            .into_iter()
            .map(|entry| -200_000 - entry.identifier_type.id.value()),
    );
    Ok(Observation { ids, total: None })
}

fn run_series_index_availability(
    database: &LibraryDatabase,
    seed: u64,
    books: u64,
) -> Result<Observation, String> {
    let series = SeriesId::new(to_i64(1 + (seed ^ 0x51_7e) % SERIES.min(books))?);
    let index = SeriesIndex::from_scaled(1_000_000).expect("fixture index is in range");
    let available_to_other = database
        .series_index_is_available(series, index, BookId::new(0))
        .map_err(display_error)?;
    let available_to_self = database
        .series_index_is_available(series, index, BookId::new(1))
        .map_err(display_error)?;
    if available_to_other || !available_to_self {
        return Err(
            "series index check did not distinguish a conflict from the current book".into(),
        );
    }
    Ok(Observation {
        ids: vec![series.value(), -1],
        total: None,
    })
}

fn run_identifier_metadata_round_trip(
    database: &mut LibraryDatabase,
) -> Result<Observation, String> {
    let book = database
        .get_book(BookId::new(1))
        .map_err(display_error)?
        .ok_or_else(|| "identifier fixture book is missing".to_owned())?;
    if book.identifiers.len() != usize::try_from(IDENTIFIERS_PER_BOOK).unwrap_or(3) {
        return Err("identifier fixture did not load every assignment".into());
    }
    let edit = BookEdit {
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
        genres: book.genres.clone(),
        identifiers: book
            .identifiers
            .iter()
            .map(|identifier| BookIdentifierEdit {
                identifier_type: IdentifierTypeReference::Existing(identifier.identifier_type.id),
                value: identifier.value.clone(),
            })
            .collect(),
    };
    database.save_book_edit(&edit).map_err(display_error)?;
    let stored = database
        .get_book(book.id)
        .map_err(display_error)?
        .ok_or_else(|| "saved identifier fixture book disappeared".to_owned())?;
    if stored.identifiers != book.identifiers {
        return Err("identifier metadata did not round-trip exactly".into());
    }
    Ok(Observation {
        ids: vec![
            book.id.value(),
            i64::try_from(stored.identifiers.len()).map_err(display_error)?,
        ],
        total: None,
    })
}

fn validate_observation(
    scenario: Scenario,
    observation: &Observation,
    books: u64,
) -> Result<(), String> {
    let unique = observation.ids.iter().copied().collect::<HashSet<_>>();
    if unique.len() != observation.ids.len() {
        return Err(format!("{} returned duplicate identities", scenario.name()));
    }
    match scenario {
        Scenario::DeepPage => {
            let expected = usize::try_from(books.saturating_sub(4_096).min(u64::from(PAGE_SIZE)))
                .map_err(display_error)?;
            if observation.ids.len() != expected {
                return Err("deep page did not contain the expected bounded window".into());
            }
        }
        Scenario::Autocomplete => {
            if observation.ids.len() > 4 * usize::try_from(AUTOCOMPLETE_LIMIT).unwrap_or(50)
                || observation.ids.first() != Some(&1)
            {
                return Err("autocomplete was unbounded or did not put selection first".into());
            }
        }
        Scenario::SeriesIndexAvailability => {
            if observation.ids.len() != 2 || observation.total.is_some() {
                return Err("series index availability did not reconcile".into());
            }
        }
        Scenario::IdentifierMetadataRoundTrip => {
            if observation.ids.len() != 2 || observation.total.is_some() {
                return Err("identifier metadata round trip did not reconcile".into());
            }
        }
        _ => {
            if observation.ids.len() > usize::try_from(PAGE_SIZE).unwrap_or(128)
                || observation.total == Some(0)
                || observation.total.is_none()
            {
                return Err("first page did not reconcile its bound and total".into());
            }
        }
    }
    Ok(())
}

fn collect_query_plans(connection: &Connection) -> Result<Vec<QueryPlan>, String> {
    let specifications = [
        (
            "contributor_filter",
            "book_contributors_contributor_role_book_idx",
            "SELECT book_id FROM book_contributors WHERE contributor_id = 1 AND role = 'author'",
        ),
        (
            "series_filter",
            "series_memberships_series_index_book_idx",
            "SELECT book_id FROM series_memberships WHERE series_id = 1",
        ),
        (
            "series_index_availability",
            "series_memberships_series_number_uidx",
            "SELECT book_id FROM series_memberships \
             WHERE series_id = 1 AND series_index = 1000000",
        ),
        (
            "tag_filter",
            "book_tags_tag_book_idx",
            "SELECT book_id FROM book_tags WHERE tag_id = 1",
        ),
        (
            "identifier_type_lookup",
            "book_identifiers_type_book_idx",
            "SELECT book_id FROM book_identifiers WHERE identifier_type_id = 1",
        ),
    ];
    specifications
        .into_iter()
        .map(|(name, required_index, sql)| {
            let mut statement = connection
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .map_err(display_error)?;
            let details = statement
                .query_map([], |row| row.get::<_, String>(3))
                .map_err(display_error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(display_error)?;
            if !details
                .iter()
                .any(|detail| detail.contains(required_index) && detail.contains("COVERING"))
            {
                return Err(format!("{name} did not use {required_index}: {details:?}"));
            }
            Ok(QueryPlan {
                name,
                required_index,
                details,
            })
        })
        .collect()
}

fn validate_seed(path: &Path, books: u64) -> Result<(), String> {
    let connection = Connection::open(path).map_err(display_error)?;
    for (table, expected) in [
        ("books", books),
        ("book_assets", books),
        ("contributors", CONTRIBUTORS.min(books)),
        ("series_entities", SERIES.min(books)),
        ("tags", TAGS.min(books)),
        ("book_tags", books * TAGS_PER_BOOK.min(books)),
        ("book_identifiers", books * IDENTIFIERS_PER_BOOK),
        ("saved_searches", SAVED_SEARCHES),
    ] {
        let count: i64 = connection
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .map_err(display_error)?;
        if u64::try_from(count).map_err(display_error)? != expected {
            return Err(format!("{table} count is {count}, expected {expected}"));
        }
    }
    connection
        .execute(
            "INSERT INTO books_fts(books_fts, rank) VALUES ('integrity-check', 1)",
            [],
        )
        .map_err(display_error)?;
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

fn to_i64(value: u64) -> Result<i64, String> {
    i64::try_from(value).map_err(display_error)
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
    use super::{Options, Scenario, nearest_rank};

    #[test]
    fn parses_query_workload_options() {
        let (command, options) = Options::parse(
            [
                "query",
                "--database",
                "library.sqlite3",
                "--output",
                "queries.json",
                "--iterations",
                "40",
                "--warmup",
                "10",
                "--fixture-version",
                "3",
            ]
            .into_iter()
            .map(std::ffi::OsString::from),
        )
        .unwrap();
        assert_eq!(command, "query");
        assert_eq!(options.iterations, 40);
        assert_eq!(options.fixture_version, 3);
        assert_eq!(Scenario::ALL.len(), 9);
    }

    #[test]
    fn nearest_rank_returns_an_observed_sample() {
        assert_eq!(nearest_rank(&[1, 2, 3, 4, 5], 50), 3);
        assert_eq!(nearest_rank(&[1, 2, 3, 4, 5], 95), 5);
    }
}
