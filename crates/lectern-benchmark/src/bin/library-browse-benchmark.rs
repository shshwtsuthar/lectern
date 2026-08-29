//! Release-mode deterministic metadata-group and scoped-book browsing workload.

use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use lectern_core::{
    BookId, LibraryGroup, LibraryGrouping, LibraryQuery, LibraryScope,
    organisation::{ContributorId, Genre, SeriesId, VirtualLibraryIcon, VirtualLibraryId},
};
use lectern_storage::LibraryDatabase;
use rusqlite::{
    Connection, Transaction, TransactionBehavior, params, params_from_iter, types::Value,
};
use serde::Serialize;

const USAGE: &str = "Usage:
  library-browse-benchmark --database PATH --output PATH [OPTIONS]

Options:
  --books N        Logical books in the fixture (default: 50000)
  --seed N         Deterministic fixture seed (default: 20260825)
  --cover-every N  Accepted for runner compatibility (default: 3)
  --iterations N   Measured iterations per scenario (default: 40)
  --warmup N       Warmup iterations per scenario (default: 10)
";

const CONTRIBUTORS: u64 = 20_000;
const SERIES: u64 = 2_500;
const VIRTUAL_LIBRARIES: u64 = 2_500;
const VIRTUAL_MEMBERSHIPS_PER_BOOK: u64 = 8;
const REPRESENTATIVE_SCOPE_BOOKS: u64 = 256;
const GROUP_PAGE_SIZE: u32 = 100;
const BOOK_PAGE_SIZE: u32 = 128;
const DEEP_PAGE_OFFSET: u64 = 1_024;

#[derive(Debug)]
struct Options {
    database: PathBuf,
    output: PathBuf,
    books: u64,
    seed: u64,
    iterations: usize,
    warmup: usize,
}

impl Options {
    fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut options = Self {
            database: PathBuf::new(),
            output: PathBuf::new(),
            books: 50_000,
            seed: 20_260_825,
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
                "--seed" => options.seed = parse_number(&name, value)?,
                "--cover-every" => {
                    let _: u64 = parse_number(&name, value)?;
                }
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

#[derive(Clone, Copy, Debug)]
enum ScenarioKind {
    VirtualLibraryGroups,
    GenreGroups,
    ContributorGroups,
    SeriesGroups,
    VirtualLibraryBooks,
    GenreBooks,
    ContributorBooks,
    SeriesBooks,
    DeepGenreWindow,
}

impl ScenarioKind {
    const ALL: [Self; 9] = [
        Self::VirtualLibraryGroups,
        Self::GenreGroups,
        Self::ContributorGroups,
        Self::SeriesGroups,
        Self::VirtualLibraryBooks,
        Self::GenreBooks,
        Self::ContributorBooks,
        Self::SeriesBooks,
        Self::DeepGenreWindow,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::VirtualLibraryGroups => "virtual_library_groups_first_page",
            Self::GenreGroups => "genre_groups_first_page",
            Self::ContributorGroups => "contributor_groups_first_page",
            Self::SeriesGroups => "series_groups_first_page",
            Self::VirtualLibraryBooks => "virtual_library_books_first_page",
            Self::GenreBooks => "genre_books_first_page",
            Self::ContributorBooks => "contributor_books_first_page",
            Self::SeriesBooks => "series_books_first_page",
            Self::DeepGenreWindow => "deep_genre_book_window_without_count",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Observation {
    Groups {
        total: u64,
        offset: u64,
        entries: Vec<GroupEntry>,
    },
    Books {
        total: Option<u64>,
        offset: u64,
        ids: Vec<BookId>,
    },
}

impl Observation {
    fn result_count(&self) -> usize {
        match self {
            Self::Groups { entries, .. } => entries.len(),
            Self::Books { ids, .. } => ids.len(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GroupEntry {
    scope: LibraryScope,
    name: String,
    description: Option<String>,
    icon: Option<String>,
    books: u64,
}

impl From<LibraryGroup> for GroupEntry {
    fn from(group: LibraryGroup) -> Self {
        Self {
            scope: group.scope,
            name: group.name,
            description: group.description,
            icon: group.icon.map(|icon| icon.as_str().to_owned()),
            books: group.books,
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
    contributors: u64,
    series: u64,
    catalog_genres: usize,
    virtual_libraries: u64,
    virtual_memberships_per_book: u64,
    group_page_size: u32,
    book_page_size: u32,
    warmup_iterations: usize,
    measured_iterations: usize,
    verified_checks: Vec<&'static str>,
    peak_rss_delta_bytes: u64,
    query_plans: Vec<QueryPlan>,
    scenarios: Vec<ScenarioResult>,
}

#[derive(Serialize)]
struct QueryPlan {
    name: &'static str,
    required_index: &'static str,
    details: Vec<String>,
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

fn run(options: &Options) -> Result<(), String> {
    if !options.database.is_file() {
        return Err(format!(
            "library-browse benchmark database is not a file: {}",
            options.database.display()
        ));
    }
    ensure_new_file(&options.output)?;
    create_parent(&options.output)?;
    validate_base_fixture(&options.database, options.books)?;
    seed_browse_fixture(&options.database, options)?;

    let reference = Connection::open(&options.database).map_err(display_error)?;
    let representative = RepresentativeScopes::new(options)?;
    let expected = ScenarioKind::ALL
        .into_iter()
        .map(|scenario| {
            reference_observation(&reference, scenario, representative)
                .map(|observation| (scenario.name(), observation))
        })
        .collect::<Result<HashMap<_, _>, _>>()?;
    let query_plans = collect_query_plans(&reference)?;
    drop(reference);

    let mut database = LibraryDatabase::open(&options.database).map_err(display_error)?;
    let baseline_rss = peak_resident_bytes()?;
    let rounds = options
        .warmup
        .checked_add(options.iterations)
        .ok_or_else(|| "browse iteration count overflowed".to_owned())?;
    let mut scenarios = Vec::with_capacity(ScenarioKind::ALL.len());

    for scenario in ScenarioKind::ALL {
        let expected = expected
            .get(scenario.name())
            .ok_or_else(|| format!("missing reference for {}", scenario.name()))?;
        let mut samples = Vec::with_capacity(options.iterations);
        let mut observed_results = 0;
        for round in 0..rounds {
            let started = Instant::now();
            let observed = production_observation(&mut database, scenario, representative)?;
            let elapsed = started.elapsed();
            validate_observation(scenario, &observed, expected)?;
            observed_results = observed.result_count();
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

    let result = BenchmarkResult {
        schema_version: 1,
        kind: "library-browse-performance",
        measured_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(display_error)?
            .as_millis(),
        database_path: options.database.display().to_string(),
        library_books: options.books,
        contributors: CONTRIBUTORS.min(options.books),
        series: SERIES.min(options.books),
        catalog_genres: Genre::ALL.len(),
        virtual_libraries: VIRTUAL_LIBRARIES.min(options.books),
        virtual_memberships_per_book: VIRTUAL_MEMBERSHIPS_PER_BOOK,
        group_page_size: GROUP_PAGE_SIZE,
        book_page_size: BOOK_PAGE_SIZE,
        warmup_iterations: options.warmup,
        measured_iterations: options.iterations,
        verified_checks: vec![
            "exact_group_identities",
            "exact_group_counts",
            "matching_book_counts",
            "stable_book_order",
            "unique_book_rows",
            "bounded_pages",
            "covering_query_plans",
        ],
        peak_rss_delta_bytes: peak_resident_bytes()?.saturating_sub(baseline_rss),
        query_plans,
        scenarios,
    };
    write_json(&options.output, &result)?;
    println!(
        "Measured {} library-browse scenarios over {} books",
        ScenarioKind::ALL.len(),
        options.books
    );
    Ok(())
}

#[derive(Clone, Copy)]
struct RepresentativeScopes {
    virtual_library: LibraryScope,
    genre: LibraryScope,
    contributor: LibraryScope,
    series: LibraryScope,
}

impl RepresentativeScopes {
    fn new(options: &Options) -> Result<Self, String> {
        let contributors = CONTRIBUTORS.min(options.books);
        let series = SERIES.min(options.books);
        let virtual_libraries = VIRTUAL_LIBRARIES.min(options.books);
        let genre_index =
            usize::try_from(options.seed % Genre::ALL.len() as u64).map_err(display_error)?;
        Ok(Self {
            virtual_library: LibraryScope::VirtualLibrary(VirtualLibraryId::new(to_i64(
                1 + options.seed % virtual_libraries,
            )?)),
            genre: LibraryScope::Genre(Genre::ALL[genre_index]),
            contributor: LibraryScope::Contributor(ContributorId::new(to_i64(
                1 + options.seed % contributors,
            )?)),
            series: LibraryScope::Series(SeriesId::new(to_i64(1 + options.seed % series)?)),
        })
    }
}

fn production_observation(
    database: &mut LibraryDatabase,
    scenario: ScenarioKind,
    scopes: RepresentativeScopes,
) -> Result<Observation, String> {
    let grouping = match scenario {
        ScenarioKind::VirtualLibraryGroups => Some(LibraryGrouping::VirtualLibraries),
        ScenarioKind::GenreGroups => Some(LibraryGrouping::Genres),
        ScenarioKind::ContributorGroups => Some(LibraryGrouping::Contributors),
        ScenarioKind::SeriesGroups => Some(LibraryGrouping::Series),
        _ => None,
    };
    if let Some(grouping) = grouping {
        let page = database
            .browse_groups(grouping, 0, GROUP_PAGE_SIZE)
            .map_err(display_error)?;
        return Ok(Observation::Groups {
            total: page.total,
            offset: page.offset,
            entries: page.groups.into_iter().map(GroupEntry::from).collect(),
        });
    }

    let scope = match scenario {
        ScenarioKind::VirtualLibraryBooks => scopes.virtual_library,
        ScenarioKind::GenreBooks | ScenarioKind::DeepGenreWindow => scopes.genre,
        ScenarioKind::ContributorBooks => scopes.contributor,
        ScenarioKind::SeriesBooks => scopes.series,
        _ => unreachable!("group scenarios returned above"),
    };
    if matches!(scenario, ScenarioKind::DeepGenreWindow) {
        return database
            .query_window_in_scope(
                &LibraryQuery::default(),
                scope,
                DEEP_PAGE_OFFSET,
                BOOK_PAGE_SIZE,
            )
            .map(|books| Observation::Books {
                total: None,
                offset: DEEP_PAGE_OFFSET,
                ids: books.into_iter().map(|book| book.id).collect(),
            })
            .map_err(display_error);
    }
    database
        .query_page_in_scope(&LibraryQuery::default(), scope, 0, BOOK_PAGE_SIZE)
        .map(|page| Observation::Books {
            total: Some(page.total),
            offset: page.offset,
            ids: page.books.into_iter().map(|book| book.id).collect(),
        })
        .map_err(display_error)
}

fn reference_observation(
    connection: &Connection,
    scenario: ScenarioKind,
    scopes: RepresentativeScopes,
) -> Result<Observation, String> {
    match scenario {
        ScenarioKind::VirtualLibraryGroups => reference_virtual_library_groups(connection),
        ScenarioKind::GenreGroups => reference_genre_groups(connection),
        ScenarioKind::ContributorGroups => reference_contributor_groups(connection),
        ScenarioKind::SeriesGroups => reference_series_groups(connection),
        ScenarioKind::VirtualLibraryBooks => {
            reference_books(connection, scopes.virtual_library, 0, true)
        }
        ScenarioKind::GenreBooks => reference_books(connection, scopes.genre, 0, true),
        ScenarioKind::ContributorBooks => reference_books(connection, scopes.contributor, 0, true),
        ScenarioKind::SeriesBooks => reference_books(connection, scopes.series, 0, true),
        ScenarioKind::DeepGenreWindow => {
            reference_books(connection, scopes.genre, DEEP_PAGE_OFFSET, false)
        }
    }
}

fn reference_virtual_library_groups(connection: &Connection) -> Result<Observation, String> {
    let total = count(connection, "SELECT count(*) FROM virtual_libraries", [])?;
    let mut statement = connection
        .prepare(
            "SELECT v.id, v.name, v.description, v.icon, ( \
                 SELECT count(*) FROM book_virtual_libraries bv \
                 WHERE bv.virtual_library_id = v.id \
             ) FROM virtual_libraries v \
             ORDER BY v.identity_key, v.id LIMIT ?1",
        )
        .map_err(display_error)?;
    let entries = statement
        .query_map([i64::from(GROUP_PAGE_SIZE)], |row| {
            Ok(GroupEntry {
                scope: LibraryScope::VirtualLibrary(VirtualLibraryId::new(row.get(0)?)),
                name: row.get(1)?,
                description: row.get(2)?,
                icon: Some(row.get(3)?),
                books: checked_count(row.get(4)?).map_err(to_sql_error)?,
            })
        })
        .map_err(display_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(display_error)?;
    Ok(Observation::Groups {
        total,
        offset: 0,
        entries,
    })
}

fn reference_genre_groups(connection: &Connection) -> Result<Observation, String> {
    let mut statement = connection
        .prepare("SELECT genre, count(*) FROM book_genres GROUP BY genre")
        .map_err(display_error)?;
    let counts = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(display_error)?
        .map(|row| {
            let (genre, books) = row.map_err(display_error)?;
            Ok((genre, checked_count(books)?))
        })
        .collect::<Result<HashMap<_, _>, String>>()?;
    let entries = Genre::ALL
        .into_iter()
        .take(usize::try_from(GROUP_PAGE_SIZE).expect("group page size fits usize"))
        .map(|genre| GroupEntry {
            scope: LibraryScope::Genre(genre),
            name: genre.to_string(),
            description: None,
            icon: None,
            books: counts.get(genre.as_str()).copied().unwrap_or(0),
        })
        .collect();
    Ok(Observation::Groups {
        total: Genre::ALL.len() as u64,
        offset: 0,
        entries,
    })
}

fn reference_contributor_groups(connection: &Connection) -> Result<Observation, String> {
    reference_named_groups(
        connection,
        "contributors",
        "SELECT c.id, c.display_name, ( \
             SELECT count(DISTINCT bc.book_id) FROM book_contributors bc \
             WHERE bc.contributor_id = c.id \
         ) FROM contributors c ORDER BY c.identity_key, c.id LIMIT ?1",
        |id| LibraryScope::Contributor(ContributorId::new(id)),
    )
}

fn reference_series_groups(connection: &Connection) -> Result<Observation, String> {
    reference_named_groups(
        connection,
        "series_entities",
        "SELECT s.id, s.name, ( \
             SELECT count(*) FROM series_memberships sm WHERE sm.series_id = s.id \
         ) FROM series_entities s ORDER BY s.identity_key, s.id LIMIT ?1",
        |id| LibraryScope::Series(SeriesId::new(id)),
    )
}

fn reference_named_groups(
    connection: &Connection,
    table: &str,
    sql: &str,
    scope: impl Fn(i64) -> LibraryScope,
) -> Result<Observation, String> {
    let total = count(connection, &format!("SELECT count(*) FROM {table}"), [])?;
    let mut statement = connection.prepare(sql).map_err(display_error)?;
    let entries = statement
        .query_map([i64::from(GROUP_PAGE_SIZE)], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(display_error)?
        .map(|row| {
            let (id, name, books) = row.map_err(display_error)?;
            Ok(GroupEntry {
                scope: scope(id),
                name,
                description: None,
                icon: None,
                books: checked_count(books)?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Observation::Groups {
        total,
        offset: 0,
        entries,
    })
}

fn reference_books(
    connection: &Connection,
    scope: LibraryScope,
    offset: u64,
    include_total: bool,
) -> Result<Observation, String> {
    let (predicate, binding) = scope_predicate(scope)?;
    let total = include_total
        .then(|| {
            count(
                connection,
                &format!("SELECT count(*) FROM books b WHERE {predicate}"),
                [binding.clone()],
            )
        })
        .transpose()?;
    let sql = format!(
        "SELECT b.id FROM books b WHERE {predicate} \
         ORDER BY b.sort_title, b.id LIMIT ?2 OFFSET ?3"
    );
    let mut statement = connection.prepare(&sql).map_err(display_error)?;
    let ids = statement
        .query_map(
            params![binding, i64::from(BOOK_PAGE_SIZE), to_i64(offset)?],
            |row| Ok(BookId::new(row.get(0)?)),
        )
        .map_err(display_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(display_error)?;
    Ok(Observation::Books { total, offset, ids })
}

fn scope_predicate(scope: LibraryScope) -> Result<(&'static str, Value), String> {
    match scope {
        LibraryScope::All => Err("benchmark scope must be narrower than All books".into()),
        LibraryScope::Contributor(id) => Ok((
            "b.id IN (SELECT book_id FROM book_contributors WHERE contributor_id = ?1)",
            id.value().into(),
        )),
        LibraryScope::Series(id) => Ok((
            "b.id IN (SELECT book_id FROM series_memberships WHERE series_id = ?1)",
            id.value().into(),
        )),
        LibraryScope::Genre(genre) => Ok((
            "b.id IN (SELECT book_id FROM book_genres WHERE genre = ?1)",
            genre.as_str().to_owned().into(),
        )),
        LibraryScope::VirtualLibrary(id) => Ok((
            "b.id IN (SELECT book_id FROM book_virtual_libraries \
             WHERE virtual_library_id = ?1)",
            id.value().into(),
        )),
    }
}

fn validate_observation(
    scenario: ScenarioKind,
    observed: &Observation,
    expected: &Observation,
) -> Result<(), String> {
    if observed != expected {
        return Err(format!(
            "{} did not match its independent reference projection",
            scenario.name()
        ));
    }
    match observed {
        Observation::Groups {
            entries, offset, ..
        } => {
            if *offset != 0 || entries.len() > GROUP_PAGE_SIZE as usize {
                return Err(format!(
                    "{} returned an invalid group page",
                    scenario.name()
                ));
            }
            let scopes = entries
                .iter()
                .map(|entry| entry.scope)
                .collect::<HashSet<_>>();
            if scopes.len() != entries.len() {
                return Err(format!(
                    "{} returned duplicate group identities",
                    scenario.name()
                ));
            }
        }
        Observation::Books { ids, offset, total } => {
            let expected_offset = if matches!(scenario, ScenarioKind::DeepGenreWindow) {
                DEEP_PAGE_OFFSET
            } else {
                0
            };
            if *offset != expected_offset || ids.len() > BOOK_PAGE_SIZE as usize {
                return Err(format!("{} returned an invalid book page", scenario.name()));
            }
            if matches!(scenario, ScenarioKind::DeepGenreWindow) != total.is_none() {
                return Err(format!(
                    "{} count behavior did not reconcile",
                    scenario.name()
                ));
            }
            if ids.iter().copied().collect::<HashSet<_>>().len() != ids.len() {
                return Err(format!("{} returned duplicate books", scenario.name()));
            }
        }
    }
    Ok(())
}

fn seed_browse_fixture(path: &Path, options: &Options) -> Result<(), String> {
    let mut connection = Connection::open(path).map_err(display_error)?;
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
        .execute_batch(
            "DELETE FROM book_virtual_libraries; \
             DELETE FROM virtual_libraries; \
             DELETE FROM book_genres;",
        )
        .map_err(display_error)?;

    seed_virtual_library_fixture(&transaction, options)?;
    seed_genre_fixture(&transaction, options)?;
    let scopes = RepresentativeScopes::new(options)?;
    seed_representative_normalized_scopes(&transaction, options, scopes)?;
    transaction.commit().map_err(display_error)?;
    validate_browse_fixture(path, options, scopes)
}

fn seed_virtual_library_fixture(
    transaction: &Transaction<'_>,
    options: &Options,
) -> Result<(), String> {
    let virtual_libraries = VIRTUAL_LIBRARIES.min(options.books);
    {
        let mut insert = transaction
            .prepare(
                "INSERT INTO virtual_libraries( \
                     id, name, identity_key, description, icon \
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .map_err(display_error)?;
        for id in 1..=virtual_libraries {
            let name = format!("Shelf {id:04}");
            let icon_index =
                usize::try_from(id).map_err(display_error)? % VirtualLibraryIcon::ALL.len();
            insert
                .execute(params![
                    to_i64(id)?,
                    name,
                    name.to_ascii_lowercase(),
                    format!("Representative virtual library {id:04}"),
                    VirtualLibraryIcon::ALL[icon_index].as_str(),
                ])
                .map_err(display_error)?;
        }
    }
    {
        let mut insert = transaction
            .prepare(
                "INSERT INTO book_virtual_libraries(book_id, virtual_library_id) \
                 VALUES (?1, ?2)",
            )
            .map_err(display_error)?;
        for book in 1..=options.books {
            for membership in 0..VIRTUAL_MEMBERSHIPS_PER_BOOK {
                let library = 1 + (book - 1 + membership * 313) % virtual_libraries;
                insert
                    .execute(params![to_i64(book)?, to_i64(library)?])
                    .map_err(display_error)?;
            }
        }
    }
    Ok(())
}

fn seed_genre_fixture(transaction: &Transaction<'_>, options: &Options) -> Result<(), String> {
    {
        let mut insert = transaction
            .prepare("INSERT INTO book_genres(book_id, genre) VALUES (?1, ?2)")
            .map_err(display_error)?;
        for book in 1..=options.books {
            let index = usize::try_from((book - 1 + options.seed) % Genre::ALL.len() as u64)
                .map_err(display_error)?;
            insert
                .execute(params![to_i64(book)?, Genre::ALL[index].as_str()])
                .map_err(display_error)?;
        }
    }
    Ok(())
}

fn seed_representative_normalized_scopes(
    transaction: &Transaction<'_>,
    options: &Options,
    scopes: RepresentativeScopes,
) -> Result<(), String> {
    if let LibraryScope::Contributor(contributor) = scopes.contributor {
        let name = format!("Contributor {:05}", contributor.value());
        let key = name.to_ascii_lowercase();
        let mut insert = transaction
            .prepare(
                "INSERT OR IGNORE INTO book_contributors( \
                     book_id, contributor_id, role, position, \
                     display_name_projection, sort_key_projection \
                 ) VALUES (?1, ?2, 'other', 99, ?3, ?4)",
            )
            .map_err(display_error)?;
        for book in 1..=REPRESENTATIVE_SCOPE_BOOKS.min(options.books) {
            insert
                .execute(params![to_i64(book)?, contributor.value(), name, key])
                .map_err(display_error)?;
        }
    }
    if let LibraryScope::Series(series) = scopes.series {
        let name = format!("Series {:04}", series.value());
        let key = name.to_ascii_lowercase();
        let final_book = REPRESENTATIVE_SCOPE_BOOKS.min(options.books);
        transaction
            .execute(
                "DELETE FROM series_memberships WHERE book_id BETWEEN 1 AND ?1",
                [to_i64(final_book)?],
            )
            .map_err(display_error)?;
        let mut insert = transaction
            .prepare(
                "INSERT INTO series_memberships( \
                     book_id, series_id, series_index, name_projection, key_projection \
                 ) VALUES (?1, ?2, NULL, ?3, ?4)",
            )
            .map_err(display_error)?;
        for book in 1..=final_book {
            insert
                .execute(params![to_i64(book)?, series.value(), name, key])
                .map_err(display_error)?;
        }
        transaction
            .execute(
                "UPDATE books SET series = ?1, series_index = NULL WHERE id BETWEEN 1 AND ?2",
                params![name, to_i64(final_book)?],
            )
            .map_err(display_error)?;
    }
    Ok(())
}

fn validate_base_fixture(path: &Path, books: u64) -> Result<(), String> {
    let database = LibraryDatabase::open(path).map_err(display_error)?;
    if database.count().map_err(display_error)? != books {
        return Err(format!("browse fixture must contain {books} logical books"));
    }
    drop(database);
    let connection = Connection::open(path).map_err(display_error)?;
    for (table, expected) in [
        ("contributors", CONTRIBUTORS.min(books)),
        ("series_entities", SERIES.min(books)),
    ] {
        if count(&connection, &format!("SELECT count(*) FROM {table}"), [])? != expected {
            return Err(format!("browse fixture {table} count did not reconcile"));
        }
    }
    Ok(())
}

fn validate_browse_fixture(
    path: &Path,
    options: &Options,
    scopes: RepresentativeScopes,
) -> Result<(), String> {
    let connection = Connection::open(path).map_err(display_error)?;
    let virtual_libraries = VIRTUAL_LIBRARIES.min(options.books);
    let expected = [
        ("virtual_libraries", virtual_libraries),
        (
            "book_virtual_libraries",
            options.books * VIRTUAL_MEMBERSHIPS_PER_BOOK,
        ),
        ("book_genres", options.books),
    ];
    for (table, expected) in expected {
        if count(&connection, &format!("SELECT count(*) FROM {table}"), [])? != expected {
            return Err(format!("browse fixture {table} count did not reconcile"));
        }
    }
    for scope in [
        scopes.virtual_library,
        scopes.genre,
        scopes.contributor,
        scopes.series,
    ] {
        let (predicate, binding) = scope_predicate(scope)?;
        let books = count(
            &connection,
            &format!("SELECT count(*) FROM books b WHERE {predicate}"),
            [binding],
        )?;
        if books < BOOK_PAGE_SIZE.into() {
            return Err(format!(
                "representative scope {scope:?} has only {books} books"
            ));
        }
    }
    Ok(())
}

fn collect_query_plans(connection: &Connection) -> Result<Vec<QueryPlan>, String> {
    let specifications = [
        (
            "contributor_scope",
            "book_contributors_contributor_role_book_idx",
            "SELECT book_id FROM book_contributors WHERE contributor_id = 1",
        ),
        (
            "series_scope",
            "series_memberships_series_index_book_idx",
            "SELECT book_id FROM series_memberships WHERE series_id = 1",
        ),
        (
            "genre_scope",
            "book_genres_genre_book_idx",
            "SELECT book_id FROM book_genres WHERE genre = 'fantasy'",
        ),
        (
            "virtual_library_scope",
            "book_virtual_libraries_library_book_idx",
            "SELECT book_id FROM book_virtual_libraries WHERE virtual_library_id = 1",
        ),
    ];
    specifications
        .into_iter()
        .map(|(name, required_index, sql)| {
            let details = connection
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                .map_err(display_error)?
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

fn count<const N: usize>(
    connection: &Connection,
    sql: &str,
    bindings: [Value; N],
) -> Result<u64, String> {
    let value = connection
        .query_row(sql, params_from_iter(bindings), |row| row.get::<_, i64>(0))
        .map_err(display_error)?;
    checked_count(value)
}

fn checked_count(value: i64) -> Result<u64, String> {
    u64::try_from(value).map_err(display_error)
}

fn to_sql_error(error: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Integer, error.into())
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

fn peak_resident_bytes() -> Result<u64, String> {
    #[cfg(target_os = "linux")]
    {
        let status = fs::read_to_string("/proc/self/status").map_err(display_error)?;
        let line = status
            .lines()
            .find(|line| line.starts_with("VmHWM:"))
            .ok_or_else(|| "VmHWM is missing from /proc/self/status".to_owned())?;
        let kib = line
            .split_whitespace()
            .nth(1)
            .ok_or_else(|| "VmHWM does not contain a value".to_owned())?
            .parse::<u64>()
            .map_err(display_error)?;
        kib.checked_mul(1_024)
            .ok_or_else(|| "peak resident byte count overflowed".to_owned())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(0)
    }
}

fn ensure_new_file(path: &Path) -> Result<(), String> {
    if path.exists() {
        return Err(format!("output already exists: {}", path.display()));
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
    fs::write(
        path,
        serde_json::to_vec_pretty(value).map_err(display_error)?,
    )
    .map_err(display_error)
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
