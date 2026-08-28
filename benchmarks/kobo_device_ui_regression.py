#!/usr/bin/env python3
"""Run the bounded GPUI Kobo-device dialog workload and retain raw samples."""

from __future__ import annotations

import argparse
import json
import math
import os
import pathlib
import subprocess
import sys
import time
from typing import Any


REPOSITORY = pathlib.Path(__file__).resolve().parents[1]
EXPECTED_MARKERS = {
    "generic_device_icon",
    "storage_presented",
    "bounded_device_listing",
    "library_correlation_presented",
    "remove_action_presented",
    "eject_action_presented",
}


def read_json(path: pathlib.Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return value


def write_json(path: pathlib.Path, value: Any) -> None:
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def nearest_rank_p95(samples: list[float]) -> float:
    if not samples:
        raise ValueError("at least one measured sample is required")
    ordered = sorted(samples)
    return ordered[max(math.ceil(len(ordered) * 0.95) - 1, 0)]


def validate_budget(budget: dict[str, Any]) -> dict[str, Any]:
    if budget.get("schema_version") != 1 or budget.get("kind") != (
        "lectern-kobo-device-ui-regression-budget"
    ):
        raise ValueError("unsupported Kobo-device UI budget")
    workload = budget.get("workload")
    if not isinstance(workload, dict) or workload.get("query_mode") != "kobo-device-ui":
        raise ValueError("budget workload must use kobo-device-ui")
    for field in ("listed_books", "managed_books", "measured_iterations"):
        if not isinstance(workload.get(field), int) or workload[field] <= 0:
            raise ValueError(f"budget workload {field} must be a positive integer")
    if not isinstance(workload.get("warmup_iterations"), int) or workload[
        "warmup_iterations"
    ] < 0:
        raise ValueError("budget workload warmup_iterations must be non-negative")
    if set(workload.get("correctness", [])) != EXPECTED_MARKERS:
        raise ValueError("budget correctness markers do not match the workload contract")
    expected_scenarios = {"initial_library_render", "device_to_painted_state"}
    if set(workload.get("scenarios", [])) != expected_scenarios:
        raise ValueError("budget scenarios do not match the workload contract")
    scenario_budgets = budget.get("budgets")
    if not isinstance(scenario_budgets, dict) or set(scenario_budgets) != expected_scenarios:
        raise ValueError("budget must define both Kobo-device UI scenarios")
    for name, scenario in scenario_budgets.items():
        if not isinstance(scenario, dict):
            raise ValueError(f"budget {name} must be an object")
        for field in ("max_p95_ms", "max_peak_rss_bytes"):
            if not isinstance(scenario.get(field), (int, float)) or scenario[field] <= 0:
                raise ValueError(f"budget {name}.{field} must be positive")
    return workload


def validate_sample(sample: dict[str, Any], workload: dict[str, Any]) -> None:
    if sample.get("schema_version") != 1 or sample.get("workload") != "kobo-device":
        raise ValueError("GPUI sample has the wrong schema or workload")
    for field in ("initial_render_ms", "device_to_paint_ms"):
        if not isinstance(sample.get(field), (int, float)) or sample[field] < 0:
            raise ValueError(f"GPUI sample field {field} must be non-negative")
    correctness = sample.get("correctness")
    if not isinstance(correctness, dict):
        raise ValueError("GPUI sample correctness must be an object")
    if correctness.get("device_name") != "Kobo eReader":
        raise ValueError("GPUI sample did not present the fallback Kobo name")
    if correctness.get("free_bytes", 0) >= correctness.get("total_bytes", 0):
        raise ValueError("GPUI sample storage values are invalid")
    if correctness.get("listed_books") != workload["listed_books"]:
        raise ValueError("GPUI sample listed-book count is incorrect")
    if correctness.get("managed_books") != workload["managed_books"]:
        raise ValueError("GPUI sample managed-book count is incorrect")
    if set(correctness.get("markers", [])) != EXPECTED_MARKERS:
        raise ValueError("GPUI sample correctness markers are incomplete")


def command(
    arguments: list[str],
    commands: list[dict[str, Any]],
    *,
    environment: dict[str, str] | None = None,
    timeout: int,
) -> None:
    started = time.monotonic()
    completed = subprocess.run(
        arguments,
        cwd=REPOSITORY,
        env=environment,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=timeout,
        check=False,
    )
    commands.append(
        {
            "command": arguments,
            "elapsed_seconds": time.monotonic() - started,
            "returncode": completed.returncode,
            "output": completed.stdout,
        }
    )
    if completed.returncode != 0:
        raise RuntimeError(f"command failed ({completed.returncode}): {' '.join(arguments)}")


def scenario_result(
    name: str, samples: list[float], peak_rss_bytes: int | None
) -> dict[str, Any]:
    return {
        "name": name,
        "samples_ms": samples,
        "latency_ms": {"p95": nearest_rank_p95(samples)},
        "peak_rss_bytes": peak_rss_bytes,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--budget", required=True, type=pathlib.Path)
    parser.add_argument("--output-dir", required=True, type=pathlib.Path)
    parser.add_argument("--baseline", type=pathlib.Path)
    parser.add_argument("--skip-build", action="store_true")
    arguments = parser.parse_args()

    budget = read_json(arguments.budget.resolve())
    workload = validate_budget(budget)
    output_directory = arguments.output_dir.resolve()
    output_directory.mkdir(parents=True, exist_ok=False)
    samples_directory = output_directory / "raw-samples"
    samples_directory.mkdir()
    commands: list[dict[str, Any]] = []
    target = pathlib.Path(os.environ.get("CARGO_TARGET_DIR", REPOSITORY / "target"))
    if not target.is_absolute():
        target = REPOSITORY / target
    executable = target / "release" / "lectern-gpui"
    if sys.platform == "win32":
        executable = executable.with_suffix(".exe")
    if not arguments.skip_build:
        command(
            [
                "cargo",
                "build",
                "--release",
                "--locked",
                "-p",
                "lectern-desktop",
                "--bin",
                "lectern-gpui",
            ],
            commands,
            timeout=1_800,
        )
    if not executable.is_file():
        raise FileNotFoundError(f"release GPUI executable is missing: {executable}")

    measured: list[dict[str, Any]] = []
    raw_paths: list[str] = []
    warmup = workload["warmup_iterations"]
    iterations = workload["measured_iterations"]
    for index in range(warmup + iterations):
        phase = "warmup" if index < warmup else "measured"
        phase_index = index if phase == "warmup" else index - warmup
        sample_path = samples_directory / f"{phase}-{phase_index:03d}.json"
        environment = os.environ.copy()
        environment.update(
            {
                "LECTERN_GPUI_BENCHMARK_OUTPUT": str(sample_path),
                "LECTERN_GPUI_BENCHMARK_WORKLOAD": "kobo-device",
            }
        )
        command([str(executable)], commands, environment=environment, timeout=15)
        sample = read_json(sample_path)
        validate_sample(sample, workload)
        raw_paths.append(str(sample_path))
        if phase == "measured":
            measured.append(sample)

    peak_samples = [
        sample["peak_rss_bytes"]
        for sample in measured
        if sample.get("peak_rss_bytes") is not None
    ]
    peak_rss = max(peak_samples) if peak_samples else None
    scenarios = [
        scenario_result(
            "initial_library_render",
            [float(sample["initial_render_ms"]) for sample in measured],
            peak_rss,
        ),
        scenario_result(
            "device_to_painted_state",
            [float(sample["device_to_paint_ms"]) for sample in measured],
            peak_rss,
        ),
    ]
    baseline = read_json(arguments.baseline.resolve()) if arguments.baseline else None
    baseline_scenarios = {
        scenario["name"]: scenario for scenario in baseline.get("scenarios", [])
    } if baseline else {}
    comparison = budget["comparison"]
    decisions = []
    for scenario in scenarios:
        limits = budget["budgets"][scenario["name"]]
        p95 = scenario["latency_ms"]["p95"]
        absolute_passed = p95 <= limits["max_p95_ms"] and (
            peak_rss is None or peak_rss <= limits["max_peak_rss_bytes"]
        )
        relative_passed = True
        baseline_p95 = None
        if scenario["name"] in baseline_scenarios:
            baseline_p95 = baseline_scenarios[scenario["name"]]["latency_ms"]["p95"]
            allowed = max(
                baseline_p95 * (1 + comparison["max_p95_regression_percent"] / 100),
                baseline_p95 + comparison["minimum_p95_delta_ms"],
            )
            relative_passed = p95 <= allowed
        decisions.append(
            {
                "scenario": scenario["name"],
                "passed": absolute_passed and relative_passed,
                "p95_ms": p95,
                "max_p95_ms": limits["max_p95_ms"],
                "peak_rss_bytes": peak_rss,
                "max_peak_rss_bytes": limits["max_peak_rss_bytes"],
                "baseline_p95_ms": baseline_p95,
                "relative_passed": relative_passed,
            }
        )
    result = {
        "kind": "lectern-kobo-device-ui-performance",
        "warmup_iterations": warmup,
        "measured_iterations": iterations,
        "raw_samples": raw_paths,
        "correctness": measured[0]["correctness"],
        "scenarios": scenarios,
        "decisions": decisions,
        "passed": all(decision["passed"] for decision in decisions),
    }
    write_json(output_directory / "result.json", result)
    write_json(output_directory / "commands.json", commands)
    print(json.dumps(result, indent=2))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
