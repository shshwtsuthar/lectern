//! Command-line entry point for Lectern.

use std::{env, ffi::OsString, process::ExitCode};

use lectern_core::BuildInfo;

const USAGE: &str = "Lectern library manager

Usage: lectern [OPTIONS]

Options:
  -h, --help       Print help
  -V, --version    Print version";

fn main() -> ExitCode {
    run(env::args_os().skip(1))
}

fn run(args: impl IntoIterator<Item = OsString>) -> ExitCode {
    let Some(argument) = args.into_iter().next() else {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    };

    match argument.to_str() {
        Some("-h" | "--help") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("-V" | "--version") => {
            let build = BuildInfo::current();
            println!("{} {}", build.name, build.version);
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!(
                "error: unrecognized argument '{}'. Run 'lectern --help' for usage.",
                argument.to_string_lossy()
            );
            ExitCode::from(2)
        }
    }
}
