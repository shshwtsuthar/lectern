#!/usr/bin/env python3
"""Run Lectern's exploratory large-library performance study."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
import os
import pathlib
import platform
import shlex
import shutil
import subprocess
import sys
import time
from typing import Any, Iterable

GIB = 1024**3
DEFAULT_CORPUS = pathlib.Path("target/benchmarks/import-corpus-v1/corpus")


class CommandRecorder:
    """Run commands while preserving enough metadata to audit a benchmark run."""

    def __init__(self, repository: pathlib.Path, output: pathlib.Path) -> None:
        self.repository = repository
        self.output = output
        self.commands: list[dict[str, Any]] = []

    def run(
        self,
        command: list[str],
        *,
        environment: dict[str, str] | None = None,
    ) -> None:
        print(f"+ {shlex.join(command)}", flush=True)
        started_at = utc_now()
        started = time.monotonic_ns()
        result = subprocess.run(
            command,
            cwd=self.repository,
            env=environment,
            check=False,
        )
        record = {
            "command": command,
            "started_at_utc": started_at,
            "elapsed_ns": time.monotonic_ns() - started,
            "return_code": result.returncode,
        }
        self.commands.append(record)
        write_json(self.output, {"commands": self.commands})
        if result.returncode != 0:
            raise RuntimeError(
                f"command failed with exit code {result.returncode}: {shlex.join(command)}"
            )


def parse_arguments(arguments: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run exploratory Lectern performance measurements without pass/fail gates."
    )
    parser.add_argument("--corpus", type=pathlib.Path, default=DEFAULT_CORPUS)
    parser.add_argument("--output-dir", type=pathlib.Path)
    parser.add_argument("--books", type=positive_int, default=50_000)
    parser.add_argument("--seed", type=non_negative_int, default=20_260_824)
    parser.add_argument("--cover-every", type=non_negative_int, default=3)
    parser.add_argument("--query-iterations", type=positive_int, default=100)
    parser.add_argument("--query-warmup", type=non_negative_int, default=10)
    parser.add_argument("--startup-runs", type=positive_int, default=3)
    parser.add_argument("--startup-idle-seconds", type=non_negative_float, default=0.5)
    parser.add_argument("--idle-seconds", type=non_negative_float, default=3.0)
    parser.add_argument("--scroll-seconds", type=positive_float, default=15.0)
    parser.add_argument("--scroll-warmup-seconds", type=non_negative_float, default=1.0)
    parser.add_argument("--scroll-pixels-per-second", type=positive_float, default=1_500.0)
    parser.add_argument("--timeout-seconds", type=positive_float, default=180.0)
    parser.add_argument("--minimum-free-gib", type=positive_float, default=40.0)
    parser.add_argument("--maximum-corpus-gib", type=positive_float, default=20.0)
    parser.add_argument("--maximum-run-gib", type=positive_float, default=20.0)
    parser.add_argument("--skip-desktop", action="store_true")
    parser.add_argument("--skip-import", action="store_true")
    parser.add_argument(
        "--smoke",
        action="store_true",
        help="Use small library/query/UI settings; corpus selection is unchanged.",
    )
    parsed = parser.parse_args(arguments)
    if parsed.scroll_warmup_seconds >= parsed.scroll_seconds:
        parser.error("--scroll-warmup-seconds must be less than --scroll-seconds")
    if parsed.smoke:
        parsed.books = min(parsed.books, 1_000)
        parsed.query_iterations = min(parsed.query_iterations, 5)
        parsed.query_warmup = min(parsed.query_warmup, 1)
        parsed.startup_runs = 1
        parsed.startup_idle_seconds = 0.2
        parsed.idle_seconds = 0.2
        parsed.scroll_seconds = 1.2
        parsed.scroll_warmup_seconds = 0.2
        parsed.timeout_seconds = max(parsed.timeout_seconds, 15.0)
    return parsed


def positive_int(value: str) -> int:
    parsed = int(value)
    if parsed <= 0:
        raise argparse.ArgumentTypeError("must be greater than zero")
    return parsed


def non_negative_int(value: str) -> int:
    parsed = int(value)
    if parsed < 0:
        raise argparse.ArgumentTypeError("must be non-negative")
    return parsed


def positive_float(value: str) -> float:
    parsed = float(value)
    if not math.isfinite(parsed) or parsed <= 0:
        raise argparse.ArgumentTypeError("must be a finite number greater than zero")
    return parsed


def non_negative_float(value: str) -> float:
    parsed = float(value)
    if not math.isfinite(parsed) or parsed < 0:
        raise argparse.ArgumentTypeError("must be a finite non-negative number")
    return parsed


def main(arguments: list[str]) -> int:
    options = parse_arguments(arguments)
    repository = pathlib.Path(__file__).resolve().parents[1]
    corpus = resolve_from_repository(repository, options.corpus)
    output = options.output_dir or default_output_directory(repository)
    output = resolve_from_repository(repository, output)
    if output.exists():
        raise RuntimeError(f"output directory already exists: {output}")
    output.mkdir(parents=True)

    corpus_inventory = inventory_corpus(corpus)
    if corpus_inventory["total_bytes"] > options.maximum_corpus_gib * GIB:
        raise RuntimeError(
            f"corpus is {corpus_inventory['total_bytes'] / GIB:.1f} GiB, above the "
            f"{options.maximum_corpus_gib:.1f} GiB safety cap"
        )
    disk_at_start = disk_snapshot(repository)
    required_free_bytes = (options.minimum_free_gib + options.maximum_run_gib) * GIB
    if disk_at_start["free_bytes"] < required_free_bytes:
        raise RuntimeError(
            f"only {disk_at_start['free_bytes'] / GIB:.1f} GiB is free; "
            f"{options.minimum_free_gib:.1f} GiB must remain free and up to "
            f"{options.maximum_run_gib:.1f} GiB is reserved for this run"
        )
    if not options.skip_desktop and not (os.environ.get("DISPLAY") or os.environ.get("WAYLAND_DISPLAY")):
        raise RuntimeError("desktop measurements require an active DISPLAY or WAYLAND_DISPLAY")

    run_metadata = {
        "schema_version": 1,
        "kind": "lectern-performance-run-metadata",
        "created_at_utc": utc_now(),
        "repository": repository_metadata(repository),
        "environment": environment_metadata(),
        "configuration": serializable_configuration(options, corpus, output),
        "corpus_inventory": corpus_inventory,
        "corpus_manifest": adjacent_corpus_manifest(corpus),
        "disk_at_start": disk_at_start,
        "measurement_definitions": measurement_definitions(),
    }
    write_json(output / "run-metadata.json", run_metadata)
    recorder = CommandRecorder(repository, output / "commands.json")

    benchmark_binary = repository / "target/release/lectern-benchmark"
    desktop_binary = repository / "target/release/lectern"
    recorder.run(
        [
            "cargo",
            "build",
            "--release",
            "--locked",
            "-p",
            "lectern-benchmark",
            "-p",
            "lectern-desktop",
        ]
    )

    library_data = output / "library-data"
    library_database = library_data / "library.sqlite3"
    seed_output = output / "seed.json"
    query_output = output / "queries.json"
    recorder.run(
        [
            str(benchmark_binary),
            "seed",
            "--database",
            str(library_database),
            "--output",
            str(seed_output),
            "--books",
            str(options.books),
            "--seed",
            str(options.seed),
            "--cover-every",
            str(options.cover_every),
        ]
    )

    startup_results: list[dict[str, Any]] = []
    scroll_result: dict[str, Any] | None = None
    if not options.skip_desktop:
        for index in range(options.startup_runs):
            result_path = output / f"startup-{index + 1:02}.json"
            run_desktop(
                recorder,
                desktop_binary,
                library_data,
                result_path,
                idle_seconds=options.startup_idle_seconds,
                scroll_seconds=0.0,
                scroll_warmup_seconds=0.0,
                scroll_pixels_per_second=options.scroll_pixels_per_second,
                timeout_seconds=options.timeout_seconds,
            )
            startup_results.append(read_completed_desktop_result(result_path))

        scroll_path = output / "scrolling.json"
        run_desktop(
            recorder,
            desktop_binary,
            library_data,
            scroll_path,
            idle_seconds=options.idle_seconds,
            scroll_seconds=options.scroll_seconds,
            scroll_warmup_seconds=options.scroll_warmup_seconds,
            scroll_pixels_per_second=options.scroll_pixels_per_second,
            timeout_seconds=options.timeout_seconds,
        )
        scroll_result = read_completed_desktop_result(scroll_path)

    recorder.run(
        [
            str(benchmark_binary),
            "query",
            "--database",
            str(library_database),
            "--output",
            str(query_output),
            "--iterations",
            str(options.query_iterations),
            "--warmup",
            str(options.query_warmup),
        ]
    )

    import_result: dict[str, Any] | None = None
    if not options.skip_import:
        import_output = output / "import.json"
        recorder.run(
            [
                str(benchmark_binary),
                "import",
                "--database",
                str(output / "import-data/library.sqlite3"),
                "--corpus",
                str(corpus),
                "--output",
                str(import_output),
            ]
        )
        import_result = read_json(import_output)

    run_bytes = directory_allocated_bytes(output)
    if run_bytes > options.maximum_run_gib * GIB:
        raise RuntimeError(
            f"run artifacts use {run_bytes / GIB:.1f} GiB, above the "
            f"{options.maximum_run_gib:.1f} GiB safety cap"
        )

    desktop_results = startup_results + ([scroll_result] if scroll_result else [])
    combined = {
        "schema_version": 1,
        "kind": "lectern-performance-results",
        "completed_at_utc": utc_now(),
        "run": run_metadata,
        "seed": read_json(seed_output),
        "startup": {
            "runs": startup_results,
            "all_process_cold_samples_ns": startup_samples(desktop_results),
            "summary_ns": summarize(startup_samples(desktop_results)),
        },
        "queries": read_json(query_output),
        "scrolling": scroll_result,
        "import": import_result,
        "memory": combined_memory(desktop_results, scroll_result, import_result),
        "disk_at_end": disk_snapshot(repository),
        "run_allocated_bytes": run_bytes,
        "warnings": [
            "Measurements are exploratory and intentionally have no pass/fail thresholds.",
            "Cold launch means a fresh process; operating-system page cache was not cleared.",
            "Frame intervals describe delivered app cadence, not GPU presentation timestamps.",
            "RSS excludes dedicated GPU memory and does not estimate total system impact.",
            "The prepared corpus repeats a bounded set of valid source publications; interpret import throughput accordingly.",
        ],
    }
    write_json(output / "results.json", combined)
    print(f"Results: {output / 'results.json'}")
    return 0


def run_desktop(
    recorder: CommandRecorder,
    binary: pathlib.Path,
    data_directory: pathlib.Path,
    output: pathlib.Path,
    *,
    idle_seconds: float,
    scroll_seconds: float,
    scroll_warmup_seconds: float,
    scroll_pixels_per_second: float,
    timeout_seconds: float,
) -> None:
    environment = os.environ.copy()
    environment.update(
        {
            "LECTERN_DATA_DIR": str(data_directory),
            "LECTERN_BENCHMARK_OUTPUT": str(output),
            "LECTERN_BENCHMARK_IDLE_SECONDS": str(idle_seconds),
            "LECTERN_BENCHMARK_SCROLL_SECONDS": str(scroll_seconds),
            "LECTERN_BENCHMARK_SCROLL_WARMUP_SECONDS": str(scroll_warmup_seconds),
            "LECTERN_BENCHMARK_SCROLL_PIXELS_PER_SECOND": str(scroll_pixels_per_second),
            "LECTERN_BENCHMARK_TIMEOUT_SECONDS": str(timeout_seconds),
            "WGPU_BACKEND": environment.get("WGPU_BACKEND", "vulkan"),
        }
    )
    recorder.run([str(binary)], environment=environment)


def read_completed_desktop_result(path: pathlib.Path) -> dict[str, Any]:
    result = read_json(path)
    if result.get("status") != "completed":
        raise RuntimeError(f"desktop benchmark did not complete: {result.get('error')}")
    return result


def startup_samples(results: Iterable[dict[str, Any]]) -> list[int]:
    return [
        int(result["startup"]["populated_library_ns"])
        for result in results
        if result and result.get("startup")
    ]


def combined_memory(
    desktop_results: list[dict[str, Any]],
    scrolling: dict[str, Any] | None,
    import_result: dict[str, Any] | None,
) -> dict[str, Any]:
    startup_peaks = [
        int(result["memory"]["startup_peak_rss_bytes"])
        for result in desktop_results
        if result["memory"].get("startup_peak_rss_bytes") is not None
    ]
    return {
        "definition": "process resident set size in bytes; dedicated GPU memory excluded",
        "startup_peak_rss_bytes": summarize(startup_peaks),
        "idle_peak_rss_bytes": nested_value(scrolling, "memory", "idle_peak_rss_bytes"),
        "scrolling_peak_rss_bytes": nested_value(
            scrolling, "memory", "scrolling_peak_rss_bytes"
        ),
        "import_peak_rss_bytes": nested_value(import_result, "memory", "peak_rss_bytes"),
    }


def nested_value(value: dict[str, Any] | None, *keys: str) -> Any:
    current: Any = value
    for key in keys:
        if not isinstance(current, dict):
            return None
        current = current.get(key)
    return current


def summarize(samples: list[int]) -> dict[str, int] | None:
    if not samples:
        return None
    ordered = sorted(samples)
    return {
        "count": len(ordered),
        "min": ordered[0],
        "mean": sum(ordered) // len(ordered),
        "p50": nearest_rank(ordered, 50),
        "p95": nearest_rank(ordered, 95),
        "p99": nearest_rank(ordered, 99),
        "max": ordered[-1],
    }


def nearest_rank(ordered: list[int], percentile: int) -> int:
    rank = math.ceil(percentile * len(ordered) / 100)
    return ordered[max(0, min(rank - 1, len(ordered) - 1))]


def inventory_corpus(corpus: pathlib.Path) -> dict[str, int | str]:
    if not corpus.is_dir():
        raise RuntimeError(f"corpus directory does not exist: {corpus}")
    counts = {"epub": 0, "pdf": 0}
    total_bytes = 0
    for path in corpus.rglob("*"):
        if not path.is_file():
            continue
        extension = path.suffix.lower().lstrip(".")
        if extension not in counts:
            continue
        counts[extension] += 1
        total_bytes += path.stat().st_size
    files = counts["epub"] + counts["pdf"]
    if files == 0:
        raise RuntimeError(f"corpus has no EPUB or PDF files: {corpus}")
    return {
        "path": str(corpus),
        "files": files,
        "epub_files": counts["epub"],
        "pdf_files": counts["pdf"],
        "total_bytes": total_bytes,
    }


def adjacent_corpus_manifest(corpus: pathlib.Path) -> dict[str, Any] | None:
    manifest = corpus.parent / "corpus_stats.json"
    return read_json(manifest) if manifest.is_file() else None


def repository_metadata(repository: pathlib.Path) -> dict[str, Any]:
    status = capture(["git", "status", "--short"], repository)
    return {
        "commit": capture(["git", "rev-parse", "HEAD"], repository),
        "branch": capture(["git", "branch", "--show-current"], repository),
        "dirty": bool(status),
        "status": status.splitlines(),
    }


def environment_metadata() -> dict[str, Any]:
    return {
        "platform": platform.platform(),
        "system": platform.system(),
        "release": platform.release(),
        "machine": platform.machine(),
        "python": platform.python_version(),
        "rustc": capture(["rustc", "-Vv"]),
        "cargo": capture(["cargo", "-V"]),
        "cpu_model": cpu_model(),
        "logical_cpus": os.cpu_count(),
        "total_memory_bytes": total_memory_bytes(),
        "load_average_at_start": list(os.getloadavg()),
        "display": os.environ.get("DISPLAY"),
        "wayland_display": os.environ.get("WAYLAND_DISPLAY"),
        "session_type": os.environ.get("XDG_SESSION_TYPE"),
        "gpu": gpu_description(),
    }


def measurement_definitions() -> dict[str, str]:
    return {
        "cold_launch": "fresh Lectern process start through the second populated-library render pass",
        "query_latency": "SQLite query, row decoding, string allocation, and full matching-result materialization on one open connection",
        "frame_interval": "monotonic interval between app frame starts after scrolling warmup",
        "egui_unstable_dt": "egui-reported interval for the current frame",
        "cpu_frame_time": "eframe CPU time for the previous app/render frame, excluding vsync wait",
        "memory": "Linux process RSS sampled from /proc at 20 ms; dedicated GPU memory excluded",
        "import": "production discovery, parallel EPUB/PDF parsing and cover generation, plus transactional persistence",
        "page_cache_control": "uncontrolled",
        "percentiles": "nearest-rank over retained raw samples",
    }


def serializable_configuration(
    options: argparse.Namespace,
    corpus: pathlib.Path,
    output: pathlib.Path,
) -> dict[str, Any]:
    return {
        key: str(value) if isinstance(value, pathlib.Path) else value
        for key, value in vars(options).items()
    } | {"corpus": str(corpus), "output_dir": str(output), "build_profile": "release"}


def cpu_model() -> str | None:
    try:
        for line in pathlib.Path("/proc/cpuinfo").read_text(encoding="utf-8").splitlines():
            if line.startswith("model name"):
                return line.split(":", 1)[1].strip()
    except OSError:
        return None
    return platform.processor() or None


def total_memory_bytes() -> int | None:
    try:
        for line in pathlib.Path("/proc/meminfo").read_text(encoding="utf-8").splitlines():
            if line.startswith("MemTotal:"):
                return int(line.split()[1]) * 1024
    except (OSError, ValueError, IndexError):
        return None
    return None


def gpu_description() -> list[str]:
    if shutil.which("lspci") is None:
        return []
    devices = capture(["lspci"], check=False).splitlines()
    labels = ("VGA compatible controller", "3D controller", "Display controller")
    return [device for device in devices if any(label in device for label in labels)]


def disk_snapshot(path: pathlib.Path) -> dict[str, int]:
    usage = shutil.disk_usage(path)
    return {"total_bytes": usage.total, "used_bytes": usage.used, "free_bytes": usage.free}


def directory_allocated_bytes(path: pathlib.Path) -> int:
    total = 0
    for root, _, files in os.walk(path):
        for filename in files:
            try:
                stat = (pathlib.Path(root) / filename).stat()
            except FileNotFoundError:
                continue
            total += stat.st_blocks * 512
    return total


def default_output_directory(repository: pathlib.Path) -> pathlib.Path:
    timestamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    return repository / "target/benchmarks/runs" / timestamp


def resolve_from_repository(repository: pathlib.Path, path: pathlib.Path) -> pathlib.Path:
    return path.resolve() if path.is_absolute() else (repository / path).resolve()


def capture(
    command: list[str],
    cwd: pathlib.Path | None = None,
    *,
    check: bool = True,
) -> str:
    result = subprocess.run(command, cwd=cwd, text=True, capture_output=True, check=check)
    return result.stdout.strip()


def read_json(path: pathlib.Path) -> dict[str, Any]:
    with path.open(encoding="utf-8") as source:
        value = json.load(source)
    if not isinstance(value, dict):
        raise RuntimeError(f"expected a JSON object in {path}")
    return value


def write_json(path: pathlib.Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    with temporary.open("w", encoding="utf-8") as destination:
        json.dump(value, destination, indent=2, sort_keys=False)
        destination.write("\n")
    temporary.replace(path)


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv[1:]))
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(2) from error
