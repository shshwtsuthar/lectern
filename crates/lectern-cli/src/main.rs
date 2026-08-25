//! Command-line diagnostics and automation for Lectern.

use std::{env, ffi::OsString, path::PathBuf, process::ExitCode};

use lectern_core::{BuildInfo, LibraryDiagnostics, LibraryService};
use lectern_service::{SqliteLibraryService, default_database_path};

const USAGE: &str = "Lectern command-line diagnostics and automation

Usage: lectern-cli [--database <PATH>] <COMMAND> [ARGS]

Commands:
  doctor                 Check database, index, relationships, and referenced files
  backup <DESTINATION>   Create a consistent SQLite snapshot at a new path
  import <PATHS...>      Discover and import EPUB/PDF publications
  scan                   Recheck referenced files and store their health
  stats                  Print compact library and asset counts

Options:
      --database <PATH>  Use an explicit library database
  -h, --help             Print help
  -V, --version          Print version";

enum Command {
    Doctor,
    Backup(PathBuf),
    Import(Vec<PathBuf>),
    Scan,
    Stats,
}

struct Invocation {
    database: PathBuf,
    command: Command,
}

enum ParseOutcome {
    Help,
    Version,
    Run(Invocation),
}

fn main() -> ExitCode {
    run(env::args_os().skip(1))
}

fn run(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    let arguments = args.into_iter().collect::<Vec<_>>();
    let outcome = match parse_arguments(&arguments) {
        Ok(outcome) => outcome,
        Err(message) => {
            eprintln!("error: {message}\n\nRun 'lectern-cli --help' for usage.");
            return ExitCode::from(2);
        }
    };

    match outcome {
        ParseOutcome::Help => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        ParseOutcome::Version => {
            let build = BuildInfo::current();
            println!("{} {}", build.name, build.version);
            ExitCode::SUCCESS
        }
        ParseOutcome::Run(invocation) => execute(invocation),
    }
}

fn parse_arguments(arguments: &[OsString]) -> Result<ParseOutcome, String> {
    if arguments.is_empty() {
        return Ok(ParseOutcome::Help);
    }

    let mut database = None;
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        match argument.to_str() {
            Some("-h" | "--help") => return Ok(ParseOutcome::Help),
            Some("-V" | "--version") => return Ok(ParseOutcome::Version),
            Some("--database") => {
                let path = arguments
                    .get(index + 1)
                    .ok_or_else(|| "--database requires a path".to_owned())?;
                if path.is_empty() {
                    return Err("--database requires a non-empty path".into());
                }
                database = Some(PathBuf::from(path));
                index += 2;
            }
            Some(value) if value.starts_with('-') => {
                return Err(format!("unrecognized option '{value}'"));
            }
            _ => break,
        }
    }

    let name = arguments
        .get(index)
        .ok_or_else(|| "a command is required".to_owned())?;
    let trailing = &arguments[index + 1..];
    let command = match name.to_str() {
        Some("doctor") => {
            require_no_arguments("doctor", trailing)?;
            Command::Doctor
        }
        Some("backup") => {
            if trailing.len() != 1 || trailing[0].is_empty() {
                return Err("backup requires exactly one destination path".into());
            }
            Command::Backup(PathBuf::from(&trailing[0]))
        }
        Some("import") => {
            if trailing.is_empty() || trailing.iter().any(|path| path.is_empty()) {
                return Err("import requires one or more paths".into());
            }
            Command::Import(trailing.iter().map(PathBuf::from).collect())
        }
        Some("scan") => {
            require_no_arguments("scan", trailing)?;
            Command::Scan
        }
        Some("stats") => {
            require_no_arguments("stats", trailing)?;
            Command::Stats
        }
        Some(value) => return Err(format!("unrecognized command '{value}'")),
        None => return Err("command names must be valid UTF-8".into()),
    };

    Ok(ParseOutcome::Run(Invocation {
        database: database.unwrap_or_else(default_database_path),
        command,
    }))
}

fn require_no_arguments(command: &str, arguments: &[OsString]) -> Result<(), String> {
    if arguments.is_empty() {
        Ok(())
    } else {
        Err(format!("{command} does not accept arguments"))
    }
}

fn execute(invocation: Invocation) -> ExitCode {
    let result = match invocation.command {
        Command::Import(paths) => execute_import(&invocation.database, &paths),
        Command::Doctor => execute_existing(&invocation.database, |service| {
            let report = service.doctor()?;
            print_doctor(&invocation.database, &report);
            Ok(if report.is_healthy() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }),
        Command::Backup(destination) => execute_existing(&invocation.database, |service| {
            let report = service.backup(&destination)?;
            println!(
                "Backed up {} books ({} bytes) to {}",
                report.books,
                report.bytes,
                report.destination.display()
            );
            Ok(ExitCode::SUCCESS)
        }),
        Command::Scan => execute_existing(&invocation.database, |service| {
            let report = service.scan_assets()?;
            println!(
                "Checked {} referenced files: {} available, {} missing, {} unreadable; {} changed",
                report.checked, report.available, report.missing, report.unreadable, report.changed
            );
            Ok(ExitCode::SUCCESS)
        }),
        Command::Stats => execute_existing(&invocation.database, |service| {
            let stats = service.stats()?;
            println!("Books: {}", stats.books);
            println!("Assets: {}", stats.assets);
            println!("Covers: {}", stats.covers);
            println!(
                "Formats: {} EPUB, {} PDF",
                stats.epub_assets, stats.pdf_assets
            );
            println!(
                "Storage: {} referenced, {} managed",
                stats.referenced_assets, stats.managed_assets
            );
            println!(
                "Health: {} unknown, {} available, {} missing, {} unreadable",
                stats.unknown_assets,
                stats.available_assets,
                stats.missing_assets,
                stats.unreadable_assets
            );
            Ok(ExitCode::SUCCESS)
        }),
    };

    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn execute_import(
    database_path: &std::path::Path,
    paths: &[PathBuf],
) -> Result<ExitCode, lectern_service::LibraryServiceError> {
    let mut service = SqliteLibraryService::open(database_path)?;
    let summary = service.import_publications(paths, &mut |_| {})?;
    println!(
        "Discovered {}; imported {}; failed {}",
        summary.discovered, summary.imported, summary.failed
    );
    for failure in &summary.failures {
        eprintln!("{}: {}", failure.path.display(), failure.message);
    }
    Ok(if summary.failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn execute_existing(
    database_path: &std::path::Path,
    operation: impl FnOnce(
        &mut SqliteLibraryService,
    ) -> Result<ExitCode, lectern_service::LibraryServiceError>,
) -> Result<ExitCode, lectern_service::LibraryServiceError> {
    let mut service = SqliteLibraryService::open_existing(database_path)?;
    operation(&mut service)
}

fn print_doctor(path: &std::path::Path, report: &LibraryDiagnostics) {
    println!("Library: {}", path.display());
    print_check(
        "Schema",
        report.schema_version == report.supported_schema_version,
        format!(
            "version {} (supported {})",
            report.schema_version, report.supported_schema_version
        ),
    );
    print_check(
        "SQLite integrity",
        report.sqlite_integrity_errors.is_empty(),
        if report.sqlite_integrity_errors.is_empty() {
            "ok".into()
        } else {
            report.sqlite_integrity_errors.join("; ")
        },
    );
    print_check(
        "Foreign keys",
        report.foreign_key_violations == 0,
        format!("{} violations", report.foreign_key_violations),
    );
    print_check(
        "Full-text index",
        report.fts_error.is_none(),
        report.fts_error.as_deref().unwrap_or("consistent"),
    );
    print_check(
        "Book assets",
        report.books_without_assets == 0
            && report.duplicate_book_formats == 0
            && report.duplicate_reference_paths == 0
            && report.invalid_asset_relationships == 0,
        format!(
            "{} books without assets, {} duplicate formats, {} duplicate paths, {} invalid relationships",
            report.books_without_assets,
            report.duplicate_book_formats,
            report.duplicate_reference_paths,
            report.invalid_asset_relationships
        ),
    );
    let files = report.referenced_files;
    print_check(
        "Referenced files",
        files.missing == 0
            && files.unreadable == 0
            && files.invalid_paths == 0
            && files.stale_health == 0,
        format!(
            "{} checked, {} available, {} missing, {} unreadable, {} invalid paths, {} stale health",
            files.checked,
            files.available,
            files.missing,
            files.unreadable,
            files.invalid_paths,
            files.stale_health
        ),
    );
    if report.unchecked_managed_assets > 0 {
        println!(
            "[SKIP] Managed files: {} assets; hash checks await managed storage",
            report.unchecked_managed_assets
        );
    } else {
        println!("[OK] Managed files: no managed assets");
    }
    println!(
        "Overall: {}",
        if report.is_healthy() {
            "healthy"
        } else {
            "issues found"
        }
    );
}

fn print_check(label: &str, passed: bool, detail: impl std::fmt::Display) {
    println!("[{}] {label}: {detail}", if passed { "OK" } else { "FAIL" });
}
