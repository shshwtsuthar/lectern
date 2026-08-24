#!/usr/bin/env python3
"""Run Lectern's deterministic storage-performance regression suites.

This deliberately measures only deterministic storage/query workloads. The broader ``run.py``
study remains an opt-in exploratory benchmark because it also needs a prepared
corpus and a native desktop session.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
import os
import pathlib
import platform
import shlex
import subprocess
import sys
import time
from typing import Any


CONFIGURATION_KIND = "lectern-query-regression-budget"
RESULT_KIND = "lectern-query-performance-regression"
SCRIPT_DIRECTORY = pathlib.Path(__file__).resolve().parent
REPOSITORY = SCRIPT_DIRECTORY.parent
DEFAULT_BUDGET = SCRIPT_DIRECTORY / "query-regression-v1.json"


class RegressionError(RuntimeError):
    """The measured workload is invalid or exceeds its approved budget."""


def parse_arguments(arguments: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run a deterministic Lectern release-storage regression suite."
    )
    parser.add_argument(
        "--budget",
        type=pathlib.Path,
        default=DEFAULT_BUDGET,
        help="versioned JSON workload and performance budget",
    )
    parser.add_argument(
        "--output-dir",
        type=pathlib.Path,
        help="new directory for command logs, raw results, and the database",
    )
    return parser.parse_args(arguments)


def main(arguments: list[str]) -> int:
    options = parse_arguments(arguments)
    budget_path = resolve_from_repository(options.budget)
    budget = load_budget(budget_path)
    output = (
        resolve_from_repository(options.output_dir)
        if options.output_dir
        else default_output_directory()
    )
    if output.exists():
        raise RegressionError(f"output directory already exists: {output}")
    output.mkdir(parents=True)

    commands: list[dict[str, Any]] = []
    report: dict[str, Any] = {
        "schema_version": 1,
        "kind": RESULT_KIND,
        "started_at_utc": utc_now(),
        "status": "running",
        "budget": {
            "path": str(budget_path),
            "schema_version": budget["schema_version"],
            "kind": budget["kind"],
        },
        "repository": repository_metadata(),
        "environment": environment_metadata(),
        "commands": commands,
    }

    try:
        workload = budget["workload"]
        database = output / "library.sqlite3"
        seed_output = output / "seed.json"
        mode = workload.get("query_mode", "full")
        result_names = {
            "full": "queries.json",
            "page": "queries.json",
            "page-covered": "queries.json",
            "remove": "removals.json",
            "detach": "detaches.json",
            "attach": "attachments.json",
            "reimport": "reimports.json",
        }
        query_output = output / result_names[mode]
        run_command(
            [
                "cargo",
                "run",
                "--release",
                "--locked",
                "-p",
                "lectern-benchmark",
                "--",
                "seed",
                "--database",
                str(database),
                "--output",
                str(seed_output),
                "--books",
                str(workload["books"]),
                "--seed",
                str(workload["seed"]),
                "--cover-every",
                str(workload["cover_every"]),
            ],
            commands,
        )
        seed = read_json(seed_output)
        validate_seed_result(seed, workload["books"])

        run_command(
            [
                "cargo",
                "run",
                "--release",
                "--locked",
                "-p",
                "lectern-benchmark",
                "--",
                {
                    "full": "query",
                    "page": "query-page",
                    "page-covered": "query-page-covered",
                    "remove": "remove",
                    "detach": "detach",
                    "attach": "attach",
                    "reimport": "reimport",
                }[mode],
                "--database",
                str(database),
                "--output",
                str(query_output),
                "--iterations",
                str(workload["measured_iterations"]),
                "--warmup",
                str(workload["warmup_iterations"]),
            ],
            commands,
        )
        query_result = read_json(query_output)
        if mode == "remove":
            decisions = evaluate_remove_result(query_result, budget)
        elif mode == "detach":
            decisions = evaluate_detach_result(query_result, budget)
        elif mode == "attach":
            decisions = evaluate_attach_result(query_result, budget)
        elif mode == "reimport":
            decisions = evaluate_reimport_result(query_result, budget)
        else:
            decisions = evaluate_query_result(query_result, budget)
        report["seed"] = seed
        report["query"] = {
            "path": str(query_output),
            "library_books": query_result["library_books"],
            "decisions": decisions,
        }
        failures = [decision for decision in decisions if not decision["passed"]]
        if failures:
            failed_names = ", ".join(decision["name"] for decision in failures)
            raise RegressionError(f"storage-performance budget exceeded: {failed_names}")
        report["status"] = "passed"
        print(f"Performance regression passed: {output / 'performance-regression.json'}")
        return 0
    except (OSError, ValueError, subprocess.SubprocessError, RegressionError) as error:
        report["status"] = "failed"
        report["error"] = str(error)
        print(f"error: {error}", file=sys.stderr)
        return 2
    finally:
        report["completed_at_utc"] = utc_now()
        write_json(output / "commands.json", {"commands": commands})
        write_json(output / "performance-regression.json", report)


def load_budget(path: pathlib.Path) -> dict[str, Any]:
    budget = read_json(path)
    return validate_budget(budget)


def validate_budget(budget: dict[str, Any]) -> dict[str, Any]:
    if budget.get("schema_version") != 1:
        raise RegressionError("budget.schema_version must be 1")
    if budget.get("kind") != CONFIGURATION_KIND:
        raise RegressionError(f"budget.kind must be {CONFIGURATION_KIND!r}")

    workload = object_field(budget, "workload", "budget")
    for field in ("books", "seed", "cover_every", "warmup_iterations", "measured_iterations"):
        positive_or_zero_field(workload, field, "budget.workload")
    if workload["books"] == 0:
        raise RegressionError("budget.workload.books must be greater than zero")
    if workload["measured_iterations"] == 0:
        raise RegressionError(
            "budget.workload.measured_iterations must be greater than zero"
        )
    query_mode = workload.get("query_mode", "full")
    if query_mode not in (
        "full",
        "page",
        "page-covered",
        "remove",
        "detach",
        "attach",
        "reimport",
    ):
        raise RegressionError(
            "budget.workload.query_mode must be 'full', 'page', 'page-covered', "
            "'remove', 'detach', 'attach', or 'reimport'"
        )
    if query_mode == "full":
        scenario_names = workload.get("full_library_scenarios")
        if not isinstance(scenario_names, list) or not all(
            isinstance(name, str) and name for name in scenario_names
        ):
            raise RegressionError(
                "budget.workload.full_library_scenarios must be a list of names"
            )
        if len(set(scenario_names)) != len(scenario_names):
            raise RegressionError(
                "budget.workload.full_library_scenarios must not repeat names"
            )
    elif query_mode in ("page", "page-covered"):
        positive_or_zero_field(workload, "page_size", "budget.workload")
        if workload["page_size"] == 0:
            raise RegressionError("budget.workload.page_size must be greater than zero")
        scenario_names = workload.get("full_count_scenarios")
        if not isinstance(scenario_names, list) or not all(
            isinstance(name, str) and name for name in scenario_names
        ):
            raise RegressionError(
                "budget.workload.full_count_scenarios must be a list of names"
            )
        if len(set(scenario_names)) != len(scenario_names):
            raise RegressionError(
                "budget.workload.full_count_scenarios must not repeat names"
            )
    else:
        if query_mode in ("remove", "detach", "attach"):
            positive_or_zero_field(workload, "page_size", "budget.workload")
            if workload["page_size"] == 0:
                raise RegressionError("budget.workload.page_size must be greater than zero")
        if query_mode == "attach":
            positive_or_zero_field(workload, "source_payload_bytes", "budget.workload")
            if workload["source_payload_bytes"] == 0:
                raise RegressionError(
                    "budget.workload.source_payload_bytes must be greater than zero"
                )
        scenario_names = workload.get("scenarios")
        if not isinstance(scenario_names, list) or not all(
            isinstance(name, str) and name for name in scenario_names
        ):
            raise RegressionError("budget.workload.scenarios must be a list of names")
        if len(set(scenario_names)) != len(scenario_names):
            raise RegressionError("budget.workload.scenarios must not repeat names")

    comparison = object_field(budget, "comparison", "budget")
    positive_or_zero_field(comparison, "paired_runs", "budget.comparison")
    if comparison["paired_runs"] == 0:
        raise RegressionError("budget.comparison.paired_runs must be greater than zero")
    positive_number_field(
        comparison,
        "max_p95_regression_percent",
        "budget.comparison",
    )
    non_negative_number_field(
        comparison,
        "minimum_p95_delta_ms",
        "budget.comparison",
    )

    budgets = object_field(budget, "budgets", "budget")
    if not budgets:
        raise RegressionError("budget.budgets must not be empty")
    for name, scenario_budget in budgets.items():
        if not isinstance(name, str) or not name:
            raise RegressionError("budget scenario names must be non-empty strings")
        if not isinstance(scenario_budget, dict):
            raise RegressionError(f"budget for {name!r} must be an object")
        positive_number_field(scenario_budget, "max_p95_ms", f"budget {name!r}")
        ratio_to = scenario_budget.get("max_p95_ratio_to")
        ratio = scenario_budget.get("max_p95_ratio")
        if (ratio_to is None) != (ratio is None):
            raise RegressionError(
                f"budget {name!r} must provide both ratio fields or neither"
            )
        if ratio_to is not None:
            if not isinstance(ratio_to, str) or ratio_to not in budgets:
                raise RegressionError(
                    f"budget {name!r}.max_p95_ratio_to must name another scenario"
                )
            positive_number_field(scenario_budget, "max_p95_ratio", f"budget {name!r}")
    unknown_scenarios = set(scenario_names).difference(budgets)
    if unknown_scenarios:
        raise RegressionError(
            "configured scenarios must have budgets: "
            + ", ".join(sorted(unknown_scenarios))
        )
    return budget


def evaluate_query_result(result: dict[str, Any], budget: dict[str, Any]) -> list[dict[str, Any]]:
    workload = budget["workload"]
    books = workload["books"]
    query_mode = workload.get("query_mode", "full")
    if positive_or_zero_field(result, "library_books", "query result") != books:
        raise RegressionError(
            f"query library count mismatch: got {result.get('library_books')}, expected {books}"
        )
    if (
        positive_or_zero_field(result, "warmup_iterations", "query result")
        != workload["warmup_iterations"]
    ):
        raise RegressionError("query warmup iteration count does not match the budget")
    if (
        positive_or_zero_field(result, "measured_iterations", "query result")
        != workload["measured_iterations"]
    ):
        raise RegressionError("query measured iteration count does not match the budget")
    if query_mode in ("page", "page-covered") and (
        positive_or_zero_field(result, "page_size", "query result")
        != workload["page_size"]
    ):
        raise RegressionError("query page size does not match the budget")

    scenarios = result.get("scenarios")
    if not isinstance(scenarios, list) or not scenarios:
        raise RegressionError("query result must contain scenarios")
    by_name: dict[str, dict[str, Any]] = {}
    for index, scenario in enumerate(scenarios):
        context = f"query scenario {index}"
        if not isinstance(scenario, dict):
            raise RegressionError(f"{context} must be an object")
        name = scenario.get("name")
        if not isinstance(name, str) or not name:
            raise RegressionError(f"{context}.name must be a non-empty string")
        if name in by_name:
            raise RegressionError(f"query result contains duplicate scenario {name!r}")
        result_count = positive_or_zero_field(scenario, "result_count", context)
        if result_count > books:
            raise RegressionError(
                f"{context} returned {result_count} rows from {books} books"
            )
        if query_mode in ("page", "page-covered"):
            if result_count > workload["page_size"]:
                raise RegressionError(
                    f"{context} returned {result_count} rows above the page size"
                )
            total_count = positive_or_zero_field(scenario, "total_count", context)
            if total_count > books:
                raise RegressionError(
                    f"{context} counted {total_count} rows from {books} books"
                )
            offset = positive_or_zero_field(scenario, "offset", context)
            if offset > total_count:
                raise RegressionError(
                    f"{context} offset {offset} exceeds its total {total_count}"
                )
        samples = scenario.get("samples_ns")
        if not isinstance(samples, list) or len(samples) != workload["measured_iterations"]:
            sample_count = len(samples) if isinstance(samples, list) else "invalid"
            raise RegressionError(
                f"{context} sample count mismatch: got {sample_count}, "
                f"expected {workload['measured_iterations']}"
            )
        if any(
            isinstance(sample, bool) or not isinstance(sample, int) or sample <= 0
            for sample in samples
        ):
            raise RegressionError(f"{context}.samples_ns must contain positive integers")
        latency = object_field(scenario, "latency_ms", context)
        positive_number_field(latency, "p95", f"{context}.latency_ms")
        by_name[name] = scenario

    expected_names = set(budget["budgets"])
    actual_names = set(by_name)
    if actual_names != expected_names:
        missing = expected_names.difference(actual_names)
        unexpected = actual_names.difference(expected_names)
        details = []
        if missing:
            details.append("missing=" + ", ".join(sorted(missing)))
        if unexpected:
            details.append("unexpected=" + ", ".join(sorted(unexpected)))
        raise RegressionError("query scenarios do not match the versioned budget: " + "; ".join(details))

    if query_mode == "full":
        for name in workload["full_library_scenarios"]:
            if by_name[name]["result_count"] != books:
                raise RegressionError(
                    f"{name} full-library result count mismatch: "
                    f"got {by_name[name]['result_count']}, expected {books}"
                )
    else:
        for name in workload["full_count_scenarios"]:
            if by_name[name]["total_count"] != books:
                raise RegressionError(
                    f"{name} full-count mismatch: "
                    f"got {by_name[name]['total_count']}, expected {books}"
                )

    return evaluate_latency_budgets(by_name, budget)


def evaluate_remove_result(
    result: dict[str, Any], budget: dict[str, Any]
) -> list[dict[str, Any]]:
    workload = budget["workload"]
    books = workload["books"]
    if positive_or_zero_field(result, "library_books", "remove result") != books:
        raise RegressionError("remove workload initial library count does not match the budget")
    if positive_or_zero_field(result, "final_library_books", "remove result") != books:
        raise RegressionError("remove workload final library count does not reconcile")
    warmup = positive_or_zero_field(result, "warmup_iterations", "remove result")
    measured = positive_or_zero_field(result, "measured_iterations", "remove result")
    if warmup != workload["warmup_iterations"] or measured != workload["measured_iterations"]:
        raise RegressionError("remove iteration counts do not match the budget")
    page_size = positive_or_zero_field(result, "page_size", "remove result")
    if page_size != workload["page_size"]:
        raise RegressionError("remove refresh page size does not match the budget")
    source_files = result.get("source_files")
    if (
        not isinstance(source_files, list)
        or len(source_files) != 2
        or not all(isinstance(path, str) and path for path in source_files)
    ):
        raise RegressionError("remove result must retain two source-file paths")
    if result.get("source_bytes_unchanged") is not True:
        raise RegressionError("remove workload did not preserve source bytes")

    scenarios = result.get("scenarios")
    if not isinstance(scenarios, list) or not scenarios:
        raise RegressionError("remove result must contain scenarios")
    by_name: dict[str, dict[str, Any]] = {}
    for index, scenario in enumerate(scenarios):
        context = f"remove scenario {index}"
        if not isinstance(scenario, dict):
            raise RegressionError(f"{context} must be an object")
        name = scenario.get("name")
        if not isinstance(name, str) or not name or name in by_name:
            raise RegressionError(f"{context}.name must be unique and non-empty")
        successful = positive_or_zero_field(scenario, "successful_removals", context)
        if successful != warmup + measured:
            raise RegressionError(f"{context} successful removal count does not reconcile")
        if positive_or_zero_field(scenario, "refreshed_total", context) != books:
            raise RegressionError(f"{context} refreshed total does not reconcile")
        expected_page = min(books, page_size)
        if positive_or_zero_field(scenario, "refreshed_result_count", context) != expected_page:
            raise RegressionError(f"{context} refreshed page count does not reconcile")
        samples = scenario.get("samples_ns")
        if not isinstance(samples, list) or len(samples) != measured:
            raise RegressionError(f"{context} sample count does not match the budget")
        if any(
            isinstance(sample, bool) or not isinstance(sample, int) or sample <= 0
            for sample in samples
        ):
            raise RegressionError(f"{context}.samples_ns must contain positive integers")
        latency = object_field(scenario, "latency_ms", context)
        positive_number_field(latency, "p95", f"{context}.latency_ms")
        by_name[name] = scenario

    expected_names = set(workload["scenarios"])
    if set(by_name) != expected_names or expected_names != set(budget["budgets"]):
        raise RegressionError("remove scenarios do not match the versioned budget")
    return evaluate_latency_budgets(by_name, budget)


def evaluate_detach_result(
    result: dict[str, Any], budget: dict[str, Any]
) -> list[dict[str, Any]]:
    workload = budget["workload"]
    books = workload["books"]
    if positive_or_zero_field(result, "library_books", "detach result") != books:
        raise RegressionError("detach workload initial library count does not match the budget")
    if positive_or_zero_field(result, "final_library_books", "detach result") != books:
        raise RegressionError("detach workload final library count does not reconcile")
    warmup = positive_or_zero_field(result, "warmup_iterations", "detach result")
    measured = positive_or_zero_field(result, "measured_iterations", "detach result")
    if warmup != workload["warmup_iterations"] or measured != workload["measured_iterations"]:
        raise RegressionError("detach iteration counts do not match the budget")
    page_size = positive_or_zero_field(result, "page_size", "detach result")
    if page_size != workload["page_size"]:
        raise RegressionError("detach refresh page size does not match the budget")
    source_files = result.get("source_files")
    if (
        not isinstance(source_files, list)
        or len(source_files) != 2
        or not all(isinstance(path, str) and path for path in source_files)
    ):
        raise RegressionError("detach result must retain two source-file paths")
    if result.get("source_bytes_unchanged") is not True:
        raise RegressionError("detach workload did not preserve source bytes")
    if result.get("metadata_preserved") is not True:
        raise RegressionError("detach workload did not preserve logical-book metadata")
    if result.get("covers_preserved") is not True:
        raise RegressionError("detach workload did not preserve cached covers")

    scenarios = result.get("scenarios")
    if not isinstance(scenarios, list) or not scenarios:
        raise RegressionError("detach result must contain scenarios")
    by_name: dict[str, dict[str, Any]] = {}
    for index, scenario in enumerate(scenarios):
        context = f"detach scenario {index}"
        if not isinstance(scenario, dict):
            raise RegressionError(f"{context} must be an object")
        name = scenario.get("name")
        if not isinstance(name, str) or not name or name in by_name:
            raise RegressionError(f"{context}.name must be unique and non-empty")
        successful = positive_or_zero_field(scenario, "successful_detaches", context)
        if successful != warmup + measured:
            raise RegressionError(f"{context} successful detach count does not reconcile")
        refreshed_total = positive_or_zero_field(scenario, "refreshed_total", context)
        if refreshed_total != books + 1:
            raise RegressionError(f"{context} refreshed total does not reconcile")
        expected_page = min(refreshed_total, page_size)
        if positive_or_zero_field(scenario, "refreshed_result_count", context) != expected_page:
            raise RegressionError(f"{context} refreshed page count does not reconcile")
        format_total = positive_or_zero_field(scenario, "format_total", context)
        expected_format_page = min(format_total, page_size)
        if positive_or_zero_field(scenario, "format_result_count", context) != expected_format_page:
            raise RegressionError(f"{context} format page count does not reconcile")
        samples = scenario.get("samples_ns")
        if not isinstance(samples, list) or len(samples) != measured:
            raise RegressionError(f"{context} sample count does not match the budget")
        if any(
            isinstance(sample, bool) or not isinstance(sample, int) or sample <= 0
            for sample in samples
        ):
            raise RegressionError(f"{context}.samples_ns must contain positive integers")
        latency = object_field(scenario, "latency_ms", context)
        positive_number_field(latency, "p95", f"{context}.latency_ms")
        by_name[name] = scenario

    expected_names = set(workload["scenarios"])
    if set(by_name) != expected_names or expected_names != set(budget["budgets"]):
        raise RegressionError("detach scenarios do not match the versioned budget")
    return evaluate_latency_budgets(by_name, budget)


def evaluate_attach_result(
    result: dict[str, Any], budget: dict[str, Any]
) -> list[dict[str, Any]]:
    workload = budget["workload"]
    books = workload["books"]
    if positive_or_zero_field(result, "library_books", "attach result") != books:
        raise RegressionError("attach workload initial library count does not match the budget")
    if positive_or_zero_field(result, "final_library_books", "attach result") != books:
        raise RegressionError("attach workload changed the logical-book count")
    warmup = positive_or_zero_field(result, "warmup_iterations", "attach result")
    measured = positive_or_zero_field(result, "measured_iterations", "attach result")
    if warmup != workload["warmup_iterations"] or measured != workload["measured_iterations"]:
        raise RegressionError("attach iteration counts do not match the budget")
    total_rounds = warmup + measured
    page_size = positive_or_zero_field(result, "page_size", "attach result")
    if page_size != workload["page_size"]:
        raise RegressionError("attach refresh page size does not match the budget")
    payload_bytes = positive_or_zero_field(result, "source_payload_bytes", "attach result")
    if payload_bytes != workload["source_payload_bytes"]:
        raise RegressionError("attach source payload does not match the budget")
    minimum_source_bytes = positive_or_zero_field(
        result, "minimum_source_bytes", "attach result"
    )
    maximum_source_bytes = positive_or_zero_field(
        result, "maximum_source_bytes", "attach result"
    )
    if minimum_source_bytes < payload_bytes or maximum_source_bytes < minimum_source_bytes:
        raise RegressionError("attach sources are smaller than the representative payload")
    source_files = result.get("source_files")
    if (
        not isinstance(source_files, list)
        or len(source_files) != total_rounds
        or not all(isinstance(path, str) and path for path in source_files)
    ):
        raise RegressionError("attach result must retain one source-file path per iteration")
    if result.get("source_bytes_unchanged") is not True:
        raise RegressionError("attach workload did not preserve source bytes")
    if result.get("metadata_preserved") is not True:
        raise RegressionError("attach workload did not preserve logical-book metadata")
    if result.get("covers_preserved") is not True:
        raise RegressionError("attach workload did not preserve cached covers")

    initial_pdf_books = positive_or_zero_field(
        result, "initial_pdf_books", "attach result"
    )
    final_pdf_books = positive_or_zero_field(result, "final_pdf_books", "attach result")
    if final_pdf_books != initial_pdf_books + total_rounds:
        raise RegressionError("attach workload PDF count does not reconcile")

    scenarios = result.get("scenarios")
    if not isinstance(scenarios, list) or not scenarios:
        raise RegressionError("attach result must contain scenarios")
    by_name: dict[str, dict[str, Any]] = {}
    for index, scenario in enumerate(scenarios):
        context = f"attach scenario {index}"
        if not isinstance(scenario, dict):
            raise RegressionError(f"{context} must be an object")
        name = scenario.get("name")
        if not isinstance(name, str) or not name or name in by_name:
            raise RegressionError(f"{context}.name must be unique and non-empty")
        validated = positive_or_zero_field(scenario, "validated_publications", context)
        attached = positive_or_zero_field(scenario, "successful_attachments", context)
        if validated != total_rounds or attached != total_rounds:
            raise RegressionError(f"{context} operation counts do not reconcile")
        if positive_or_zero_field(scenario, "refreshed_total", context) != final_pdf_books:
            raise RegressionError(f"{context} refreshed total does not reconcile")
        expected_page = min(final_pdf_books, page_size)
        if positive_or_zero_field(scenario, "refreshed_result_count", context) != expected_page:
            raise RegressionError(f"{context} refreshed page count does not reconcile")
        samples = scenario.get("samples_ns")
        if not isinstance(samples, list) or len(samples) != measured:
            raise RegressionError(f"{context} sample count does not match the budget")
        if any(
            isinstance(sample, bool) or not isinstance(sample, int) or sample <= 0
            for sample in samples
        ):
            raise RegressionError(f"{context}.samples_ns must contain positive integers")
        latency = object_field(scenario, "latency_ms", context)
        positive_number_field(latency, "p95", f"{context}.latency_ms")
        by_name[name] = scenario

    expected_names = set(workload["scenarios"])
    if set(by_name) != expected_names or expected_names != set(budget["budgets"]):
        raise RegressionError("attach scenarios do not match the versioned budget")
    return evaluate_latency_budgets(by_name, budget)


def evaluate_reimport_result(
    result: dict[str, Any], budget: dict[str, Any]
) -> list[dict[str, Any]]:
    workload = budget["workload"]
    books = workload["books"]
    if positive_or_zero_field(result, "library_books", "re-import result") != books:
        raise RegressionError("re-import workload initial library count does not match the budget")
    if positive_or_zero_field(result, "final_library_books", "re-import result") != books:
        raise RegressionError("re-import workload changed the logical-book count")
    warmup = positive_or_zero_field(result, "warmup_iterations", "re-import result")
    measured = positive_or_zero_field(result, "measured_iterations", "re-import result")
    if warmup != workload["warmup_iterations"] or measured != workload["measured_iterations"]:
        raise RegressionError("re-import iteration counts do not match the budget")
    if result.get("metadata_preserved") is not True:
        raise RegressionError("re-import workload did not preserve logical-book metadata")
    if result.get("assets_preserved") is not True:
        raise RegressionError("re-import workload did not preserve file assets")
    if result.get("covers_preserved") is not True:
        raise RegressionError("re-import workload did not preserve cached covers")

    scenarios = result.get("scenarios")
    if not isinstance(scenarios, list) or not scenarios:
        raise RegressionError("re-import result must contain scenarios")
    by_name: dict[str, dict[str, Any]] = {}
    for index, scenario in enumerate(scenarios):
        context = f"re-import scenario {index}"
        if not isinstance(scenario, dict):
            raise RegressionError(f"{context} must be an object")
        name = scenario.get("name")
        if not isinstance(name, str) or not name or name in by_name:
            raise RegressionError(f"{context}.name must be unique and non-empty")
        successful = positive_or_zero_field(scenario, "successful_reimports", context)
        if successful != warmup + measured:
            raise RegressionError(f"{context} successful re-import count does not reconcile")
        samples = scenario.get("samples_ns")
        if not isinstance(samples, list) or len(samples) != measured:
            raise RegressionError(f"{context} sample count does not match the budget")
        if any(
            isinstance(sample, bool) or not isinstance(sample, int) or sample <= 0
            for sample in samples
        ):
            raise RegressionError(f"{context}.samples_ns must contain positive integers")
        latency = object_field(scenario, "latency_ms", context)
        positive_number_field(latency, "p95", f"{context}.latency_ms")
        by_name[name] = scenario

    expected_names = set(workload["scenarios"])
    if set(by_name) != expected_names or expected_names != set(budget["budgets"]):
        raise RegressionError("re-import scenarios do not match the versioned budget")
    return evaluate_latency_budgets(by_name, budget)


def evaluate_latency_budgets(
    by_name: dict[str, dict[str, Any]], budget: dict[str, Any]
) -> list[dict[str, Any]]:
    p95_by_name = {
        name: float(scenario["latency_ms"]["p95"]) for name, scenario in by_name.items()
    }
    decisions = []
    for name in sorted(budget["budgets"]):
        scenario_budget = budget["budgets"][name]
        p95_ms = p95_by_name[name]
        maximum_ms = float(scenario_budget["max_p95_ms"])
        decision: dict[str, Any] = {
            "name": name,
            "p95_ms": p95_ms,
            "max_p95_ms": maximum_ms,
            "passed": p95_ms <= maximum_ms,
        }
        if "max_p95_ratio_to" in scenario_budget:
            reference_name = scenario_budget["max_p95_ratio_to"]
            reference_p95 = p95_by_name[reference_name]
            if reference_p95 <= 0:
                raise RegressionError(
                    f"reference scenario {reference_name!r} has a non-positive p95"
                )
            ratio = p95_ms / reference_p95
            maximum_ratio = float(scenario_budget["max_p95_ratio"])
            decision |= {
                "p95_ratio_to": reference_name,
                "p95_ratio": ratio,
                "max_p95_ratio": maximum_ratio,
            }
            decision["passed"] = bool(decision["passed"] and ratio <= maximum_ratio)
        decisions.append(decision)
    return decisions


def run_command(command: list[str], commands: list[dict[str, Any]]) -> None:
    print(f"+ {shlex.join(command)}", flush=True)
    started_at = utc_now()
    started = time.monotonic_ns()
    result = subprocess.run(command, cwd=REPOSITORY, check=False)
    record = {
        "command": command,
        "started_at_utc": started_at,
        "elapsed_ns": time.monotonic_ns() - started,
        "return_code": result.returncode,
    }
    commands.append(record)
    if result.returncode != 0:
        raise RegressionError(
            f"command failed with exit code {result.returncode}: {shlex.join(command)}"
        )


def validate_seed_result(result: dict[str, Any], expected_books: int) -> None:
    requested = positive_or_zero_field(result, "requested_books", "seed result")
    stored = positive_or_zero_field(result, "stored_books", "seed result")
    if requested != expected_books or stored != expected_books:
        raise RegressionError(
            "seed count mismatch: "
            f"requested={requested}, stored={stored}, expected={expected_books}"
        )


def object_field(value: dict[str, Any], key: str, context: str) -> dict[str, Any]:
    field = value.get(key)
    if not isinstance(field, dict):
        raise RegressionError(f"{context}.{key} must be an object")
    return field


def positive_or_zero_field(value: dict[str, Any], key: str, context: str) -> int:
    field = value.get(key)
    if isinstance(field, bool) or not isinstance(field, int) or field < 0:
        raise RegressionError(f"{context}.{key} must be a non-negative integer")
    return field


def positive_number_field(value: dict[str, Any], key: str, context: str) -> float:
    field = value.get(key)
    if isinstance(field, bool) or not isinstance(field, (int, float)):
        raise RegressionError(f"{context}.{key} must be a finite number greater than zero")
    number = float(field)
    if not math.isfinite(number) or number <= 0:
        raise RegressionError(f"{context}.{key} must be a finite number greater than zero")
    return number


def non_negative_number_field(value: dict[str, Any], key: str, context: str) -> float:
    field = value.get(key)
    if isinstance(field, bool) or not isinstance(field, (int, float)):
        raise RegressionError(f"{context}.{key} must be a finite non-negative number")
    number = float(field)
    if not math.isfinite(number) or number < 0:
        raise RegressionError(f"{context}.{key} must be a finite non-negative number")
    return number


def read_json(path: pathlib.Path) -> dict[str, Any]:
    with path.open(encoding="utf-8") as source:
        value = json.load(source)
    if not isinstance(value, dict):
        raise RegressionError(f"expected a JSON object in {path}")
    return value


def write_json(path: pathlib.Path, value: Any) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    with temporary.open("w", encoding="utf-8") as destination:
        json.dump(value, destination, indent=2)
        destination.write("\n")
    temporary.replace(path)


def resolve_from_repository(path: pathlib.Path) -> pathlib.Path:
    return path.resolve() if path.is_absolute() else (REPOSITORY / path).resolve()


def default_output_directory() -> pathlib.Path:
    timestamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    return REPOSITORY / "target/benchmarks/query-regression" / f"{timestamp}-{os.getpid()}"


def repository_metadata() -> dict[str, str | bool | None]:
    status = capture(["git", "status", "--short"])
    return {
        "commit": capture(["git", "rev-parse", "HEAD"]),
        "branch": capture(["git", "branch", "--show-current"]),
        "dirty": bool(status),
    }


def environment_metadata() -> dict[str, str | int | None]:
    return {
        "platform": platform.platform(),
        "machine": platform.machine(),
        "python": platform.python_version(),
        "rustc": capture(["rustc", "-Vv"]),
        "cargo": capture(["cargo", "-V"]),
        "logical_cpus": os.cpu_count(),
    }


def capture(command: list[str]) -> str | None:
    try:
        result = subprocess.run(
            command,
            cwd=REPOSITORY,
            text=True,
            capture_output=True,
            check=False,
        )
    except OSError:
        return None
    return result.stdout.strip() if result.returncode == 0 else None


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
