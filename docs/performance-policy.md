# Performance policy

Lectern treats responsiveness and bounded resource use as product contracts. Performance-sensitive
changes must carry objective evidence before they are committed and must pass the repository's
automated performance gate before merge.

## Change classification

Every pull request must declare one of these classifications:

- **None:** documentation, comments, non-production tests, or static copy that cannot change runtime
  behavior.
- **Potential:** a change that may affect execution frequency, generated code, dependencies, layout,
  allocation, I/O, concurrency, caching, or data volume.
- **Material:** a new or changed hot path, algorithm, schema/query, importer, renderer, worker model,
  cache, or performance workload.

The automated classifier is deliberately conservative. Runtime Rust, Cargo dependency/profile,
toolchain, benchmark, and performance-workflow changes require the performance gate. A contributor
may classify additional changes as sensitive, but must not downgrade a path selected by automation.
UI work is exempt only when it cannot alter runtime layout, rendering, repainting, texture handling,
or interaction work.

CI rejects a missing, ambiguous, or downgraded declaration. Potential and Material declarations
must acknowledge the applicable deterministic coverage, budgets, and retained evidence before the
benchmark job starts.

## Evidence required before commit

Before every commit containing a performance-sensitive change:

1. Name the affected user journey and workload.
2. Run the fastest relevant release-mode p95 regression suite.
3. Add a deterministic scenario first when the behavior is not represented by an existing suite.
4. Retain raw samples and verify the scenario's correctness checks, not only its timing.
5. Record the command and outcome in the pull request.

Storage and query changes currently require all registered deterministic storage suites:

```sh
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
  --budget benchmarks/organisation-migration-regression-v3.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/organisation-query-regression-v2.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/organisation-vocabulary-regression-v2.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/bulk-tags-regression-v2.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/bulk-remove-regression-v2.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/saved-searches-regression-v2.json
```

GPUI bootstrap, theme, component, asset, and empty-library rendering changes require the native
empty-library suite:

```sh
python3 benchmarks/performance_regression.py \
  --budget benchmarks/ui-bootstrap-regression-v1.json
```

It uses release-mode fresh processes, retains 40 measured samples after 5 warmups, verifies exact
ready and busy presentation markers, and gates first-frame and click-to-painted-busy-state p95 plus
peak RSS. It requires an active X11 or Wayland display.

GPUI populated-library selection, selected-card rendering, contextual actions, destructive
confirmation, and their theme tokens require the native selection suite in addition to the atomic
bulk-removal storage suite:

```sh
python3 benchmarks/performance_regression.py \
  --budget benchmarks/ui-selection-regression-v1.json
python3 benchmarks/performance_regression.py \
  --budget benchmarks/bulk-remove-regression-v2.json
```

The selection suite retains 40 measured fresh-process samples after 5 warmups for a 50,000-book
projection with a bounded 128-card first page. It verifies the compact selection descriptor and
exact confirmation copy while gating populated first paint, selection-to-painted-context, and
confirmation-to-painted-modal p95 plus peak RSS.

GPUI single-book selection, detail loading, metadata-editor layout, and book-asset presentation
require the native book-detail suite:

```sh
python3 benchmarks/performance_regression.py \
  --budget benchmarks/ui-book-detail-regression-v2.json
```

It retains 40 measured fresh-process samples after 5 warmups for the same 50,000-book projection
and bounded 128-card first page. The representative detail fixture includes ordered contributor
roles, a series index, multiple tags, publication date, a 3.5-star rating, ordinary publication
metadata, and both EPUB and PDF assets. It verifies the complete panel markers while gating
populated first paint, book-selection-to-painted-sidebar p95, and peak RSS.

The normalized organisation v2 fixture replaces the historical duplicate series-number
distribution with deterministic unique exact numbers at the same 50,000-book, 2,500-series scale.
Its query suite includes the indexed conflict/self-exclusion check used by the Book number input.
The version-five-to-current migration suite reaches schema version 9 and measures installation of
the partial unique index plus the canonical publication-date and half-star-rating detail table;
unit migration coverage separately supplies deliberate version-seven duplicates, verifies the
deterministic repair, and covers version-eight metadata defaults. Keeping these detail-only values
outside the hot `books` rows preserves the compact indexed projection used by grid and search scans.

The export suite is also mandatory for changes to copy buffers, publication, overwrite behavior,
export scheduling, or export progress. Import, startup, scrolling, rendering, or memory changes
must also run the applicable workload from
the exploratory harness. When that workload is not yet stable enough to gate, report its before and
after raw results and add or improve deterministic coverage rather than claiming an exemption.
The compositor-backed desktop workload checks title, author, and recently-added sort-to-first-
painted-frame p95 against a 50 ms product budget while retaining all interaction samples.
The bulk-tag suite additionally requires a native display and checks selection dispatch and durable
completion through their next painted frames; CI supplies an isolated X11 display for this gate.

## Merge gate

Performance-sensitive pull requests are measured against their base revision on the same runner.
The checked-in storage suites use three base/candidate runs and compare the median run-level p95 so a
single noisy process does not decide a merge.
Each comparable scenario must pass two independent controls:

- the absolute p95 budget, which protects the user experience even when the base is already slow;
- the relative p95 budget, which prevents small regressions from accumulating below a loose
  absolute ceiling.

A relative regression fails only when it exceeds both the allowed percentage and the scenario's
minimum material latency delta. This avoids treating sub-millisecond timer noise as product impact.
New versioned scenarios without a base equivalent are held to their absolute budget until a
comparable baseline exists.

The required `Performance gate` job must be enabled in branch protection. Documentation-only pull
requests pass the classifier without spending time on release benchmarks. Scheduled and manually
dispatched workflows always run the absolute suites against the current revision.

## Budget governance

Benchmark workloads and limits are versioned product contracts.

- Do not reduce data size, iterations, assertions, scenario coverage, or measurement scope to obtain
  a pass.
- Do not increase a limit in the same commit as the feature that exceeds it.
- A budget relaxation requires repeat measurements, a user-impact explanation, alternatives
  considered, and explicit approval from the repository owner.
- Material scenario-definition changes require a new workload version so historical results remain
  interpretable.
- Raw results must include the commit, dirty state, toolchain, host metadata, commands, samples, and
  correctness reconciliation.

## Measurement standards

- Use optimized, locked builds and production code paths.
- Use deterministic, versioned datasets at representative scale.
- Warm up the measured path and retain enough independent samples to make p95 meaningful.
- Compare base and candidate on the same hardware and software image whenever possible.
- Treat hosted-runner variance, scheduling, thermal state, filesystem cache state, and display/GPU
  differences as measurement inputs, not excuses to raise budgets.
- Use p95 latency for interactive and batch-duration tails, missed-frame rate and p95/p99 frame time
  for rendering, throughput tails for import work, and peak plus steady-state measurements for
  memory.

## Cadence

- **Every performance-sensitive commit:** fastest applicable local release benchmark.
- **Every performance-sensitive pull request:** automated base-versus-candidate gate.
- **Weekly and on demand:** all deterministic absolute-budget suites with retained artifacts.
- **Before a release:** full query, startup, scrolling, import, and memory study on representative
  hardware.

A blocked or waived performance check is a blocked change. Any exceptional waiver must be explicit,
time-limited, approved by the repository owner, and linked to remediation work.
