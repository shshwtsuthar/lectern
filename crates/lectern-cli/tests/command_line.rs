//! End-to-end tests for the Lectern command-line boundary.

use std::process::Command;

fn lectern() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lectern"))
}

#[test]
fn prints_help() {
    let output = lectern().arg("--help").output().expect("run lectern");

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: lectern"));
    assert!(output.stderr.is_empty());
}

#[test]
fn prints_version() {
    let output = lectern().arg("--version").output().expect("run lectern");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("Lectern {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn rejects_unknown_arguments() {
    let output = lectern().arg("--unknown").output().expect("run lectern");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unrecognized argument"));
}
