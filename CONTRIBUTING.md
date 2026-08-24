# Contributing

## Development workflow

1. Install the toolchain selected by `rust-toolchain.toml` with rustup.
2. Create a focused branch and keep changes scoped to one concern.
3. Add or update tests with behavior changes.
4. Run the local quality gate before opening a pull request:

   ```sh
   cargo fmt --all --check
   cargo clippy-all
   cargo test-all
   RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked
   ```

   If `cargo-deny` is installed, also run `cargo deny check` to validate advisories, licenses,
   duplicate versions, and dependency sources. CI always runs this check.

5. For a performance-sensitive storage or query change, also run:

   ```sh
   python3 benchmarks/performance_regression.py
   ```

   Compare the retained JSON output before changing any versioned performance budget.

6. Explain user-visible behavior, risks, and validation in the pull request.

## Engineering conventions

- Keep `lectern-core` independent of UI frameworks, databases, operating-system APIs, and devices.
- Put integrations behind narrow interfaces owned by the layer that consumes them.
- Return structured errors from libraries; presentation and process exit are adapter concerns.
- Avoid `unsafe`. Any future exception requires an architecture decision record, a documented
  safety invariant, and focused tests.
- Prefer explicit data migrations and backward-compatible formats for durable user data.
- Record decisions with broad or difficult-to-reverse consequences in `docs/adr/`.

## Commits and releases

Use imperative commit subjects and include the reason for non-obvious changes. The project follows
semantic versioning once a public API or release channel exists. Update `CHANGELOG.md` for notable
user-facing changes.
