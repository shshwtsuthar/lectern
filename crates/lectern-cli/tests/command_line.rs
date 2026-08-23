//! End-to-end tests for the Lectern command-line boundary.

use std::process::Command;

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
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: lectern-cli"));
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
    assert!(String::from_utf8_lossy(&output.stderr).contains("unrecognized argument"));
}
