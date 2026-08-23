# Lectern exploratory performance study

This directory contains an opt-in, non-gating benchmark workflow for large Lectern libraries.
It exercises the production SQLite query path, EPUB/PDF importer, and native egui cover grid.
Raw JSON is the comparison interface; the workflow intentionally does not define CI pass/fail
thresholds while the baseline is being established.

## Workloads

- A deterministic 50,000-book SQLite library, with stable metadata and one 320×480 JPEG cover
  for every three books by default.
- Seven interleaved search, format-filter, and sort scenarios, retaining 100 measured samples
  per scenario after warmup.
- Fresh desktop processes for populated-library startup measurements.
- A deterministic sustained cover-grid scroll with wall-frame, egui interval, and CPU-frame
  samples after a warmup window.
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

Use `python3 benchmarks/run.py --help` to change library size, repetitions, scroll duration,
corpus path, output directory, or disk guards. `--smoke` bounds the library and UI settings for a
quick end-to-end check but still uses the selected corpus unless `--skip-import` is supplied.

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
| `scrolling.json` | Startup, idle, scrolling, frame samples, display scale, cover state, and phase RSS |
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
- Startup, post-population idle-window, scrolling, and import memory are Linux process RSS. RSS is
  sampled every 20 ms and excludes dedicated GPU memory.
- Percentiles use the nearest-rank method and every underlying sample remains in JSON.

The runner performs count and reconciliation checks—such as seeded books, UI books, query sample
counts, discovered imports, failures, and stored imports—but treats latency, frame time, and memory
as observations rather than pass/fail gates.

## Comparing runs

Compare runs made with the same library seed, cover frequency, corpus fingerprint, build profile,
display scale, and measurement settings. Retain host and repository metadata alongside any quoted
number. With only three startup repetitions, p95 and p99 collapse to an observed maximum; report
the individual startup samples and range rather than implying a stable tail estimate.

Before sharing a conclusion, recompute percentiles from the raw nanosecond arrays and reconcile
the JSON counts. Treat differences near timer noise, scheduler variance, thermal effects, or page
cache state as hypotheses for another run, not regressions.
