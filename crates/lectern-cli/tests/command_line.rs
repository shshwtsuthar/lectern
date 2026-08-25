//! End-to-end tests for the Lectern command-line boundary.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("lectern-cli-{label}-{}-{id}", std::process::id()));
        fs::create_dir(&path).expect("create CLI test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove CLI test directory");
    }
}

fn lectern_cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lectern-cli"))
}

#[test]
fn prints_help() {
    let output = lectern_cli()
        .arg("--help")
        .output()
        .expect("run lectern-cli");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: lectern-cli"));
    assert!(stdout.contains("doctor"));
    assert!(stdout.contains("backup <DESTINATION>"));
    assert!(output.stderr.is_empty());
}

#[test]
fn prints_version() {
    let output = lectern_cli()
        .arg("--version")
        .output()
        .expect("run lectern-cli");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("Lectern {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn rejects_unknown_arguments() {
    let output = lectern_cli()
        .arg("--unknown")
        .output()
        .expect("run lectern-cli");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unrecognized option"));
}

#[test]
fn stats_and_doctor_operate_on_an_explicit_library() {
    let directory = TestDirectory::new("diagnostics");
    let database = directory.path().join("library.sqlite3");
    let missing_import = directory.path().join("not-present.epub");
    let create = lectern_cli()
        .args([
            "--database",
            database.to_str().expect("UTF-8 test path"),
            "import",
        ])
        .arg(&missing_import)
        .output()
        .expect("create empty library through import workflow");
    assert!(
        create.status.success(),
        "{}",
        String::from_utf8_lossy(&create.stderr)
    );

    let stats = lectern_cli()
        .args([
            "--database",
            database.to_str().expect("UTF-8 test path"),
            "stats",
        ])
        .output()
        .expect("run stats");
    assert!(stats.status.success());
    assert!(String::from_utf8_lossy(&stats.stdout).contains("Books: 0"));

    let doctor = lectern_cli()
        .args([
            "--database",
            database.to_str().expect("UTF-8 test path"),
            "doctor",
        ])
        .output()
        .expect("run doctor");
    assert!(
        doctor.status.success(),
        "{}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    let stdout = String::from_utf8_lossy(&doctor.stdout);
    assert!(stdout.contains("[OK] SQLite integrity"));
    assert!(stdout.contains("Overall: healthy"));
}

#[test]
fn backup_creates_a_snapshot_without_overwriting_it() {
    let directory = TestDirectory::new("backup");
    let database = directory.path().join("library.sqlite3");
    let destination = directory.path().join("backup.sqlite3");
    let missing_import = directory.path().join("not-present.epub");
    let create = lectern_cli()
        .args([
            "--database",
            database.to_str().expect("UTF-8 test path"),
            "import",
        ])
        .arg(missing_import)
        .output()
        .expect("create empty library");
    assert!(create.status.success());

    let backup = lectern_cli()
        .args([
            "--database",
            database.to_str().expect("UTF-8 test path"),
            "backup",
        ])
        .arg(&destination)
        .output()
        .expect("create backup");
    assert!(
        backup.status.success(),
        "{}",
        String::from_utf8_lossy(&backup.stderr)
    );
    assert!(destination.is_file());

    let repeat = lectern_cli()
        .args([
            "--database",
            database.to_str().expect("UTF-8 test path"),
            "backup",
        ])
        .arg(&destination)
        .output()
        .expect("repeat backup");
    assert_eq!(repeat.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&repeat.stderr).contains("already exists"));
}

#[test]
fn administrative_commands_do_not_create_a_missing_library() {
    let directory = TestDirectory::new("missing");
    let database = directory.path().join("absent.sqlite3");

    let output = lectern_cli()
        .args([
            "--database",
            database.to_str().expect("UTF-8 test path"),
            "doctor",
        ])
        .output()
        .expect("run doctor");

    assert_eq!(output.status.code(), Some(1));
    assert!(!database.exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not exist"));
}
