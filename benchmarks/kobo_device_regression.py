#!/usr/bin/env python3
"""Run and validate Lectern's deterministic Kobo-device performance workload."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import pathlib
import platform
import subprocess
import sys
from typing import Any


SCRIPT_DIRECTORY = pathlib.Path(__file__).resolve().parent
REPOSITORY = SCRIPT_DIRECTORY.parent
DEFAULT_BUDGET = SCRIPT_DIRECTORY / "kobo-device-regression-v1.json"


class RegressionError(RuntimeError):
    """The workload was invalid or exceeded its approved budget."""


def main(arguments: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--budget", type=pathlib.Path, default=DEFAULT_BUDGET)
    parser.add_argument("--output-dir", type=pathlib.Path)
    options = parser.parse_args(arguments)
    budget_path = resolve(options.budget)
    budget = read_json(budget_path)
    validate_budget(budget)
    output = resolve(options.output_dir) if options.output_dir else default_output_dir()
    if output.exists():
        raise RegressionError(f"output directory already exists: {output}")
    output.mkdir(parents=True)
    raw_output = output / "kobo-device.json"
    workload = budget["workload"]
    command = [
        "cargo",
        "run",
        "--release",
        "--locked",
        "-p",
        "lectern-device",
        "--example",
        "kobo_device_benchmark",
        "--",
        "--output",
        str(raw_output),
        "--books",
        str(workload["books"]),
        "--warmup",
        str(workload["warmup_iterations"]),
        "--iterations",
        str(workload["measured_iterations"]),
    ]
    started = utc_now()
    completed = subprocess.run(command, cwd=REPOSITORY, text=True, capture_output=True)
    command_record = {
        "command": command,
        "started_at_utc": started,
        "completed_at_utc": utc_now(),
        "exit_code": completed.returncode,
        "stdout": completed.stdout,
        "stderr": completed.stderr,
    }
    write_json(output / "commands.json", {"commands": [command_record]})
    report: dict[str, Any] = {
        "schema_version": 1,
        "kind": "lectern-device-performance-regression",
        "started_at_utc": started,
        "completed_at_utc": utc_now(),
        "status": "running",
        "budget": {"path": str(budget_path), "contents": budget},
        "repository": repository_metadata(),
        "environment": environment_metadata(),
        "raw_result": str(raw_output),
    }
    try:
        if completed.returncode != 0:
            raise RegressionError(f"benchmark command failed with {completed.returncode}")
        result = read_json(raw_output)
        decisions = evaluate(result, budget)
        report["decisions"] = decisions
        failed = [decision["name"] for decision in decisions if not decision["passed"]]
        if failed:
            raise RegressionError("device-performance budget exceeded: " + ", ".join(failed))
        report["status"] = "passed"
        print(f"Kobo device performance regression passed: {output}")
        return 0
    except (OSError, ValueError, RegressionError) as error:
        report["status"] = "failed"
        report["error"] = str(error)
        print(f"error: {error}", file=sys.stderr)
        return 2
    finally:
        report["completed_at_utc"] = utc_now()
        write_json(output / "performance-regression.json", report)


def validate_budget(budget: dict[str, Any]) -> None:
    if budget.get("schema_version") != 1:
        raise RegressionError("budget.schema_version must be 1")
    if budget.get("kind") != "lectern-device-regression-budget":
        raise RegressionError("budget.kind must be lectern-device-regression-budget")
    workload = budget.get("workload")
    if not isinstance(workload, dict):
        raise RegressionError("budget.workload must be an object")
    for field in (
        "books",
        "source_bytes_per_book",
        "candidate_volumes",
        "warmup_iterations",
        "measured_iterations",
    ):
        if not isinstance(workload.get(field), int) or workload[field] < 0:
            raise RegressionError(f"budget.workload.{field} must be a non-negative integer")
    if workload["books"] == 0 or workload["measured_iterations"] == 0:
        raise RegressionError("books and measured_iterations must be greater than zero")
    if not isinstance(budget.get("budgets"), dict):
        raise RegressionError("budget.budgets must be an object")


def evaluate(result: dict[str, Any], budget: dict[str, Any]) -> list[dict[str, Any]]:
    workload = budget["workload"]
    if result.get("schema_version") != 1 or result.get("workload") != "kobo-device-v1":
        raise RegressionError("unexpected benchmark result identity")
    for field in ("books", "source_bytes_per_book", "warmup_iterations", "measured_iterations"):
        if result.get(field) != workload[field]:
            raise RegressionError(f"result.{field} does not match the budget")
    system_enumeration = result.get("system_enumeration")
    reconciliation = result.get("reconciliation")
    transfer = result.get("transfer")
    if (
        not isinstance(system_enumeration, dict)
        or not isinstance(reconciliation, dict)
        or not isinstance(transfer, dict)
    ):
        raise RegressionError("benchmark result sections are missing")
    expected_system = {
        "production_volume_provider_exercised",
        "stable_system_sample_count",
    }
    expected_reconciliation = {
        "ordinary_volumes_rejected",
        "single_marker_volume_detected",
        "stable_reconciliation_count",
    }
    expected_transfer = {
        "all_books_transferred",
        "exact_source_hashes_preserved",
        "no_partial_files_retained",
        "history_reconciled",
    }
    if set(system_enumeration.get("correctness", [])) != expected_system:
        raise RegressionError("system-enumeration correctness markers are incomplete")
    if set(reconciliation.get("correctness", [])) != expected_reconciliation:
        raise RegressionError("reconciliation correctness markers are incomplete")
    if set(transfer.get("correctness", [])) != expected_transfer:
        raise RegressionError("transfer correctness markers are incomplete")
    if reconciliation.get("candidate_volumes") != workload["candidate_volumes"]:
        raise RegressionError("candidate-volume count does not match the workload")
    if reconciliation.get("detected_devices") != 1:
        raise RegressionError("benchmark must detect exactly one Kobo")
    if transfer.get("transferred_books_per_iteration") != workload["books"]:
        raise RegressionError("not every planned book was transferred")
    expected_bytes = workload["books"] * workload["source_bytes_per_book"]
    if transfer.get("transferred_bytes_per_iteration") != expected_bytes:
        raise RegressionError("transferred byte count does not match the workload")
    if len(transfer.get("samples_ns", [])) != workload["measured_iterations"]:
        raise RegressionError("transfer raw sample count does not match the workload")
    if len(system_enumeration.get("samples_ns", [])) < 40:
        raise RegressionError("system enumeration must retain at least 40 raw samples")
    if len(reconciliation.get("samples_ns", [])) < 40:
        raise RegressionError("reconciliation must retain at least 40 raw samples")
    limits = budget["budgets"]
    rss = transfer.get("peak_rss_delta_bytes")
    return [
        decision(
            "system_enumeration_p95_ms",
            float(system_enumeration["p95_ms"]),
            float(limits["system_enumeration"]["max_p95_ms"]),
            "maximum",
        ),
        decision(
            "reconciliation_p95_ms",
            float(reconciliation["p95_ms"]),
            float(limits["reconciliation"]["max_p95_ms"]),
            "maximum",
        ),
        decision(
            "batch_transfer_p95_ms",
            float(transfer["p95_ms"]),
            float(limits["batch_transfer"]["max_p95_ms"]),
            "maximum",
        ),
        decision(
            "batch_transfer_p05_throughput_mib_per_second",
            float(transfer["p05_throughput_mib_per_second"]),
            float(limits["batch_transfer"]["min_p05_throughput_mib_per_second"]),
            "minimum",
        ),
        {
            "name": "batch_transfer_peak_rss_delta_bytes",
            "observed": rss,
            "limit": limits["batch_transfer"]["max_peak_rss_delta_bytes"],
            "comparison": "maximum",
            "passed": rss is None
            or int(rss) <= limits["batch_transfer"]["max_peak_rss_delta_bytes"],
        },
    ]


def decision(name: str, observed: float, limit: float, comparison: str) -> dict[str, Any]:
    return {
        "name": name,
        "observed": observed,
        "limit": limit,
        "comparison": comparison,
        "passed": observed <= limit if comparison == "maximum" else observed >= limit,
    }


def repository_metadata() -> dict[str, Any]:
    commit = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=REPOSITORY, text=True, capture_output=True
    )
    dirty = subprocess.run(
        ["git", "status", "--porcelain"], cwd=REPOSITORY, text=True, capture_output=True
    )
    return {
        "commit": commit.stdout.strip() if commit.returncode == 0 else None,
        "dirty": bool(dirty.stdout.strip()) if dirty.returncode == 0 else None,
    }


def environment_metadata() -> dict[str, Any]:
    return {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "rustc": command_text(["rustc", "--version"]),
        "cargo": command_text(["cargo", "--version"]),
        "logical_cpus": os.cpu_count(),
    }


def command_text(command: list[str]) -> str | None:
    result = subprocess.run(command, text=True, capture_output=True)
    return result.stdout.strip() if result.returncode == 0 else None


def resolve(path: pathlib.Path | None) -> pathlib.Path:
    if path is None:
        raise RegressionError("path is required")
    return path if path.is_absolute() else REPOSITORY / path


def default_output_dir() -> pathlib.Path:
    stamp = dt.datetime.now(dt.UTC).strftime("%Y%m%d-%H%M%S-%f")
    return REPOSITORY / "target" / "benchmarks" / "kobo-device-regression" / stamp


def read_json(path: pathlib.Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise RegressionError(f"expected JSON object in {path}")
    return value


def write_json(path: pathlib.Path, value: dict[str, Any]) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def utc_now() -> str:
    return dt.datetime.now(dt.UTC).isoformat()


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv[1:]))
    except RegressionError as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(2) from error
