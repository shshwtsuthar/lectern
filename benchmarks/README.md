# Lectern performance measurements

This directory contains two complementary workflows:

- `performance_regression.py` is a deterministic, versioned regression runner. It exercises backup
  and diagnostics, full-result and paged queries, normalized curation, and book/asset lifecycle
  operations each week in GitHub Actions, can be dispatched manually, and fails when a release
  latency or resource budget is exceeded.
- `run.py` is an opt-in exploratory study for large libraries. It exercises the production SQLite
  query path, EPUB/PDF importer, and native egui cover grid. Its raw JSON is intentionally
  non-gating because the workload requires a prepared corpus and a graphical desktop session.

## Storage regression suites

Each regression workload seeds a 50,000-book SQLite library with deterministic metadata, runs 10
warmup iterations followed by 40 measured iterations of every versioned query scenario, and checks
both result integrity and p95 latency. It records all raw samples, the database, toolchain/host
metadata, commands, and a compact pass/fail report.

The maintenance workload in [`maintenance-regression-v1.json`](maintenance-regression-v1.json)
runs 3 warmups and 20 measured iterations of the production online-backup and diagnostic paths. It
validates SQLite, foreign keys, FTS, relationships, referenced-file partitioning, snapshot counts,
snapshot integrity, and collision safety while gating backup and doctor p95.

The metadata-only full-result workload and its limits live in
[`query-regression-v1.json`](query-regression-v1.json), with a matching covered-library projection
in [`query-covered-regression-v1.json`](query-covered-regression-v1.json). The original
bounded-window workload lives in [`query-page-regression-v1.json`](query-page-regression-v1.json):
its first page includes the matching count, its deep title page checks late-window access, and its
filtered page exercises FTS with the format join. The covered-library bounded workload in
[`query-page-covered-regression-v1.json`](query-page-covered-regression-v1.json) additionally gates
the first author and recently-added pages that users request from the desktop. The budget is
intentionally tied to the workload version: adding, removing, or materially changing a scenario
requires an explicit review of the configuration rather than silently dropping it from the
performance suite. Each full-result asset-health filter also has a relative budget against its full
title sort, which catches a disproportionate regression even if a runner becomes slower overall.

The single-book lifecycle workload lives in
[`remove-book-regression-v1.json`](remove-book-regression-v1.json). It repeatedly adds a covered
logical book with EPUB and PDF assets to a 50,000-book library, measures its durable removal plus the
first bounded library refresh, and verifies the book, cover, and search entry are gone while both
source files remain byte-for-byte unchanged.

The format-attachment lifecycle workload lives in
[`attach-format-regression-v1.json`](attach-format-regression-v1.json). It selects covered EPUB-only
books from a 50,000-book library, validates a distinct 8 MiB PDF for each iteration, atomically
attaches it, and refreshes the first PDF-filtered page. It verifies the logical-book count, metadata,
cover, existing asset identity, unique filtered result, and source bytes after every attachment.

The detach lifecycle workload lives in
[`detach-asset-regression-v1.json`](detach-asset-regression-v1.json). It repeatedly installs a
covered two-format logical book, detaches its PDF relationship, and refreshes both the first library
page and PDF filter. It verifies the EPUB relationship, metadata, cover, and both source files remain
unchanged before cleaning up the candidate outside the measured interval.

The replacement lifecycle workload lives in
[`replace-asset-regression-v1.json`](replace-asset-regression-v1.json). It validates a representative
8 MiB PDF, deliberately replaces a healthy referenced PDF while retaining the asset ID, and
refreshes the first library page. It verifies metadata, cover, identity, both source files, and the
50,000-book library count before cleaning up each candidate outside the measured interval.

The publication-export workload lives in
[`export-asset-regression-v1.json`](export-asset-regression-v1.json). It copies a deterministic
256 MiB source through the production bounded-buffer engine, retains first-progress and full-copy
samples, gates p05 throughput and peak RSS growth, and verifies exact bytes, collision safety,
missing-source handling, and temporary-file cleanup.

The normalized organisation workloads cover migration, exact/fielded query planning, and bounded
vocabulary reads and mutations. The bulk-tag gate in
[`bulk-tags-regression-v1.json`](bulk-tags-regression-v1.json) selects 10,000 matching books without
materializing them, applies and reverses an atomic tag edit, checks exact relationship and memory
results, and retains 40 native compositor samples for both dispatch and completion-to-painted-grid.
It requires an active X11 or Wayland display; CI runs it under an isolated X11 display.

The bulk-removal gate in
[`bulk-remove-regression-v1.json`](bulk-remove-regression-v1.json) resolves a compact query-backed
selection of 10,000 books and removes it in one transaction before refreshing the first bounded
library page. Independent fixture copies retain 40 raw samples while verifying exact cascades for
assets, covers, organisation relationships, and FTS; unchanged vocabulary and saved searches;
rollback and stale-selection safety; bounded memory; and byte-for-byte preservation of a real
publication source file. Durable removal plus the first refreshed page has a 3.5 second p95 budget,
and additional process RSS is capped at 32 MiB.

The saved-search gate in
[`saved-searches-regression-v1.json`](saved-searches-regression-v1.json) uses 250 complete canonical
projections. It measures a bounded 100-row manager page, application through the first 128 matching
book IDs, and create/update/rename/delete lifecycle while proving that book, vocabulary, asset, and
FTS state remains intact.

The GPUI bootstrap workload in
[`ui-bootstrap-regression-v1.json`](ui-bootstrap-regression-v1.json) launches the optimized
`lectern-gpui` binary in 5 warmup and 40 measured fresh processes. It retains each process's raw
sample, verifies the exact empty-library and busy-state copy, measures main entry to the first
painted frame and Add-books dispatch to the painted busy state, and gates p95 latency plus peak RSS.
It requires an active X11 or Wayland display.

The GPUI selection workload in
[`ui-selection-regression-v1.json`](ui-selection-regression-v1.json) launches the same optimized
binary with a deterministic 50,000-book projection and a bounded 128-card first page. It retains 40
measured fresh-process samples after 5 warmups, verifies that selection remains a compact descriptor,
and measures the initial populated-library paint, selection dispatch through the painted contextual
bar, and destructive-confirmation dispatch through its painted modal. The exact removal transaction
and refreshed first page remain covered by `bulk-remove-regression-v1`.

Performance-sensitive pull requests additionally run the base and candidate revisions three times
each on the same runner. The gate compares the median of their run-level p95 values and fails a
scenario when it exceeds both the versioned percentage limit and minimum material latency delta.
Using three paired runs reduces the influence of a single noisy process while retaining every raw
sample. A newly versioned workload that does not exist on the base revision runs against its
absolute budget until it has a comparable history.

Run it locally from the repository root:

```bash
python3 benchmarks/performance_regression.py \
  --budget benchmarks/maintenance-regression-v1.json
python3 benchmarks/performance_regression.py
python3 benchmarks/performance_regression.py \
  --budget benchmarks/query-covered-regression-v1.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/query-page-regression-v1.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/query-page-covered-regression-v1.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/remove-book-regression-v1.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/attach-format-regression-v1.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/detach-asset-regression-v1.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/replace-asset-regression-v1.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/export-asset-regression-v1.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/reimport-known-path-regression-v1.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/organisation-migration-regression-v1.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/organisation-query-regression-v1.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/organisation-vocabulary-regression-v1.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/bulk-tags-regression-v1.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/bulk-remove-regression-v1.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/saved-searches-regression-v1.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/ui-bootstrap-regression-v1.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/ui-selection-regression-v1.json
```

Use `--output-dir PATH` to choose a new artifact directory, or `--budget PATH` to evaluate a
proposed versioned workload. The runner refuses to reuse an output directory. Its default output is
under `target/benchmarks/query-regression/`, and the final status is in
`performance-regression.json` alongside `queries.json`, `seed.json`, `commands.json`, and the
SQLite database.

GitHub Actions runs the same command every Monday at 03:17 UTC on `ubuntu-24.04`; it is also
available through **Run workflow**. The artifact is retained for 90 days even if a budget fails.
Hosted runners have unavoidable variance, so adjust a limit only after comparing retained raw
artifacts from several runs and understanding the change. Do not raise a threshold merely to make
a failure disappear.

The `Performance gate` check also runs on pull requests selected by the conservative changed-path
classifier. It always reports a stable pass/fail status for branch protection; documentation-only
changes pass without building the release benchmark.

Registered CI suites live in [`suites-v1.json`](suites-v1.json). Add a versioned budget there when a
new deterministic gate is introduced; the orchestration runner executes registered suites rather
than duplicating their commands in workflow YAML. Repository administrators must configure
`Performance gate` as a required branch-protection check.

## Exploratory study

## Workloads

- A deterministic 50,000-book SQLite library, with stable metadata and one 320×480 JPEG cover
  for every three books by default.
- Eight interleaved search, format-filter, asset-health-filter, and sort scenarios, retaining 100
  measured samples per scenario after warmup.
- Fresh desktop processes for populated-library startup measurements.
- A deterministic sustained cover-grid scroll with wall-frame, egui interval, and CPU-frame
  samples after a warmup window.
- Forty round-robin title, author, and recently-added refreshes, measuring each request through the
  first presented sorted frame against a 50 ms p95 product budget.
- Forty Open and Reveal dispatches through the desktop's injected no-op platform adapter, measuring
  each request through the next painted frame against the same 50 ms p95 product budget.
- A byte-pinned 10,000-file corpus containing 7,000 EPUB and 3,000 PDF files for production-path
  discovery, parsing, cover generation, and persistence.

The import corpus repeats 48 valid source publications across a controlled size distribution.
It measures a synthetic parser and persistence workload, not 10,000 unique titles. See
[`import-corpus-v1/README.md`](import-corpus-v1/README.md) for provenance and rights notes.

## Prepare and run

The desktop measurements require Linux with `/proc`, an active Wayland or X11 session, and a
working native graphics backend. The corpus recipe additionally requires `curl`, `qpdf`, and
`unzip`.

From the repository root:

```bash
benchmarks/import-corpus-v1/prepare.sh --check-manifest
benchmarks/import-corpus-v1/prepare.sh
python3 benchmarks/run.py
```

Use `python3 benchmarks/run.py --help` to change library size, repetitions, sort iterations, scroll
duration, corpus path, output directory, or disk guards. `--smoke` bounds the library and UI
settings for a quick end-to-end check but still uses the selected corpus unless `--skip-import` is
supplied.

The runner refuses an existing output directory. By default it also:

- rejects a corpus over 20 GiB;
- reserves up to 20 GiB for run artifacts;
- requires at least 40 GiB to remain free;
- builds both benchmark binaries in locked release mode; and
- gives each instrumented desktop process a 30-second external grace period beyond its internal
  timeout.

## Result files

Each run is written below `target/benchmarks/runs/` unless `--output-dir` is provided.

| File | Contents |
| --- | --- |
| `results.json` | Comparison-ready merged result and all retained summaries |
| `run-metadata.json` | Commit, dirty state, host, display, GPU, configuration, definitions, and corpus fingerprint |
| `commands.json` | Commands, elapsed time, exit status, and timeout state |
| `seed.json` | Fixture counts, seed, cover dimensions/bytes, elapsed time, and database size |
| `startup-*.json` | Per-process startup and phase RSS measurements |
| `scrolling.json` | Startup, idle, sort-to-paint, scrolling, frame samples, display scale, cover state, and phase RSS |
| `queries.json` | Raw query latency samples and p50/p95/p99 summaries |
| `import.json` | Corpus inspection, progress, throughput, failures, database size, and peak RSS |

The run directory also contains the seeded and imported SQLite databases so findings can be
inspected. These generated artifacts are ignored by Git.

## Measurement definitions

- Startup begins at Rust `main` entry and ends after the second populated-library UI pass. It is a
  fresh-process measurement; dynamic-loader time is excluded and the operating-system page cache
  is not cleared.
- Query latency includes SQLite execution, row decoding, string allocation, and complete
  materialization of the matching result set on one open connection.
- Frame interval is the monotonic time between delivered app frame starts after scrolling warmup.
  `egui_unstable_dt` and the previous frame's eframe CPU time are retained separately. These are
  application timings, not GPU presentation timestamps.
- Sort-to-first-painted-frame begins immediately before the benchmark applies the same query change
  as a sort selection and ends on the app frame after the matching first page was installed. This
  ensures one populated frame has been presented. Each sort retains 40 samples and must remain at
  or below 50 ms p95.
- Asset-action-to-first-painted-frame begins when the compositor workload queues an Open or Reveal
  request through a no-op platform adapter and ends after the following frame. It excludes external
  application startup; each action retains 40 samples and must remain at or below 50 ms p95.
- Bulk selection dispatch ends after the busy state is painted; bulk completion ends after the
  refreshed result page has been installed and painted. Each endpoint retains 40 samples and has a
  50 ms p95 budget, while the paired 10,000-book durable mutation and refresh has a 500 ms p95 and
  32 MiB peak-RSS-growth budget.
- Saved-search manager latency includes the bounded alphabetical read and full projection decode;
  apply latency includes exact count plus the first 128 IDs; lifecycle latency covers canonical
  create, list, update, rename, and delete with integrity reconciliation after every sample.
- Startup, post-population idle-window, sorting, scrolling, and import memory are Linux process RSS.
  RSS is sampled every 20 ms and excludes dedicated GPU memory.
- Percentiles use the nearest-rank method and every underlying sample remains in JSON.

The runner performs count and reconciliation checks—such as seeded books, stable first results per
sort, UI books, query sample counts, discovered imports, failures, and stored imports. The scripted
sort interaction has a checked 50 ms p95 budget; the other exploratory latency, frame-time, and
memory measurements remain observations rather than pass/fail gates.

## Comparing runs

Compare runs made with the same library seed, cover frequency, corpus fingerprint, build profile,
display scale, and measurement settings. Retain host and repository metadata alongside any quoted
number. With only three startup repetitions, p95 and p99 collapse to an observed maximum; report
the individual startup samples and range rather than implying a stable tail estimate.

Before sharing a conclusion, recompute percentiles from the raw nanosecond arrays and reconcile
the JSON counts. Treat differences near timer noise, scheduler variance, thermal effects, or page
cache state as hypotheses for another run, not regressions.
