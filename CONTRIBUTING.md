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

5. Classify the pull request's performance impact using
   [`docs/performance-policy.md`](docs/performance-policy.md). Runtime Rust, dependency/profile,
   storage, query, import, worker, cache, rendering, and benchmark changes are performance-sensitive.
   Before every commit containing a performance-sensitive storage or query change, run:

   ```sh
   python3 benchmarks/performance_regression.py
   python3 benchmarks/performance_regression.py \
     --budget benchmarks/query-page-regression-v1.json
   ```

   Compare retained raw output against the base revision. Other performance-sensitive paths require
   their applicable workload from `benchmarks/run.py`; add deterministic coverage when none exists.
   Never relax a versioned performance budget merely to make a change pass.

6. Explain user-visible behavior, risks, performance classification, and validation in the pull
   request. The required performance workflow automatically gates runtime changes.

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
