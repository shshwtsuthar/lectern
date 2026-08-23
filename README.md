# Lectern

Lectern is intended to become a fast, native-feeling library manager for people who own ebooks.
This repository currently contains the production-oriented Rust scaffold, not product features.

## Quick start

Install Rust with [rustup](https://rustup.rs/). The workspace supports Rust 1.97 or newer and uses
the latest stable toolchain for day-to-day development.

```sh
cargo run
cargo run -p lectern-cli -- --help
cargo test-all
cargo clippy-all
cargo fmt --all --check
```

## Workspace

```text
crates/
├── lectern-core/     # UI- and infrastructure-independent application boundary
├── lectern-desktop/  # Native application
├── lectern-import/   # EPUB discovery and ingestion
├── lectern-storage/  # SQLite persistence
└── lectern-cli/      # Command-line diagnostics and automation
docs/
└── adr/           # Architecture decision records
```

The workspace starts with no third-party runtime dependencies. Add dependencies at the workspace
root when multiple crates share them, and keep adapter-specific dependencies in their owning crate.
The eventual desktop toolkit, persistence engine, and async runtime are deliberately undecided.

## Quality gates

Every pull request is expected to pass formatting, Clippy with warnings denied, tests on Linux,
macOS, and Windows, documentation checks, an MSRV build, and the dependency policy in `deny.toml`.
CI definitions live in `.github/workflows/ci.yml`.

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development workflow and
[docs/architecture.md](docs/architecture.md) for dependency rules.

## License

No distribution license has been selected yet. Add one before distributing the project outside
its authorized contributors.
