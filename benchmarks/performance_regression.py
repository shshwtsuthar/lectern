#!/usr/bin/env python3
"""Run Lectern's deterministic performance regression suites.

Most suites measure deterministic storage/query workloads. The bulk-tag suite also exercises its
versioned compositor endpoints and therefore requires a native desktop session.
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
import sqlite3
import subprocess
import sys
import time
from typing import Any


CONFIGURATION_KIND = "lectern-query-regression-budget"
ORGANISATION_CONFIGURATION_KIND = "lectern-organisation-regression-budget"
UI_CONFIGURATION_KIND = "lectern-ui-regression-budget"
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
        mode = workload.get("query_mode", "full")
        if mode in ("ui-bootstrap", "ui-selection", "ui-book-detail"):
            query_output = output / f"{mode}.json"
            if mode == "ui-bootstrap":
                ui_result = run_ui_bootstrap(query_output, output, workload, commands)
                decisions = evaluate_ui_bootstrap_result(ui_result, budget)
            elif mode == "ui-selection":
                ui_result = run_ui_selection(query_output, output, workload, commands)
                decisions = evaluate_ui_selection_result(ui_result, budget)
            else:
                ui_result = run_ui_book_detail(query_output, output, workload, commands)
                decisions = evaluate_ui_book_detail_result(ui_result, budget)
            report["seed"] = {
                "requested_books": workload["books"],
                "stored_books": workload["books"],
                "metadata_seed": workload["seed"],
                "cover_every": workload["cover_every"],
            }
            report["query"] = {
                "path": str(query_output),
                "library_books": workload["books"],
                "decisions": decisions,
            }
            failures = [decision for decision in decisions if not decision["passed"]]
            if failures:
                failed_names = ", ".join(decision["name"] for decision in failures)
                raise RegressionError(f"UI-performance budget exceeded: {failed_names}")
            report["status"] = "passed"
            print(f"Performance regression passed: {output / 'performance-regression.json'}")
            return 0

        database = output / "library.sqlite3"
        seed_output = output / "seed.json"
        result_names = {
            "full": "queries.json",
            "page": "queries.json",
            "page-covered": "queries.json",
            "remove": "removals.json",
            "detach": "detaches.json",
            "attach": "attachments.json",
            "replace": "replacements.json",
            "export": "exports.json",
            "reimport": "reimports.json",
            "organisation-migration": "migrations.json",
            "organisation-query": "organisation-queries.json",
            "organisation-vocabulary": "organisation-vocabulary.json",
            "bulk-tags": "organisation-bulk-tags.json",
            "bulk-remove": "organisation-bulk-remove.json",
            "saved-searches": "organisation-saved-searches.json",
            "maintenance": "maintenance.json",
        }
        query_output = output / result_names[mode]
        run_command(seed_command(mode, database, seed_output, workload), commands)
        seed = read_json(seed_output)
        if mode == "organisation-migration":
            validate_migration_seed_result(seed, workload)
        elif mode in (
            "organisation-query",
            "organisation-vocabulary",
            "bulk-tags",
            "bulk-remove",
            "saved-searches",
        ):
            validate_organisation_query_seed_result(seed, workload)
        else:
            validate_seed_result(seed, workload["books"])

        run_command(workload_command(mode, database, query_output, workload), commands)
        query_result = read_json(query_output)
        desktop_result: dict[str, Any] | None = None
        if mode == "remove":
            decisions = evaluate_remove_result(query_result, budget)
        elif mode == "detach":
            decisions = evaluate_detach_result(query_result, budget)
        elif mode == "attach":
            decisions = evaluate_attach_result(query_result, budget)
        elif mode == "replace":
            decisions = evaluate_replace_result(query_result, budget)
        elif mode == "export":
            decisions = evaluate_export_result(query_result, budget)
        elif mode == "reimport":
            decisions = evaluate_reimport_result(query_result, budget)
        elif mode == "organisation-migration":
            decisions = evaluate_migration_result(query_result, budget)
        elif mode == "organisation-query":
            decisions = evaluate_organisation_query_result(query_result, budget)
        elif mode == "organisation-vocabulary":
            decisions = evaluate_organisation_vocabulary_result(query_result, budget)
        elif mode == "bulk-tags":
            decisions = evaluate_bulk_tag_result(query_result, budget)
            desktop_output = output / "organisation-bulk-tags-desktop.json"
            run_bulk_tag_desktop(
                database,
                desktop_output,
                workload,
                commands,
            )
            desktop_result = read_json(desktop_output)
            decisions += evaluate_bulk_tag_desktop_result(desktop_result, budget)
        elif mode == "bulk-remove":
            decisions = evaluate_bulk_remove_result(query_result, budget)
        elif mode == "saved-searches":
            decisions = evaluate_saved_search_result(query_result, budget)
        elif mode == "maintenance":
            decisions = evaluate_maintenance_result(query_result, budget)
        else:
            decisions = evaluate_query_result(query_result, budget)
        report["seed"] = seed
        report["query"] = {
            "path": str(query_output),
            "library_books": query_result["library_books"],
            "decisions": decisions,
        }
        if desktop_result is not None:
            report["desktop"] = {
                "path": str(desktop_output),
                "result": desktop_result,
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


def seed_command(
    mode: str,
    database: pathlib.Path,
    output: pathlib.Path,
    workload: dict[str, Any],
) -> list[str]:
    command = ["cargo", "run", "--release", "--locked", "-p", "lectern-benchmark"]
    if mode == "organisation-migration":
        command += ["--bin", "organisation-benchmark", "--", "seed-migration"]
    elif mode in (
        "organisation-query",
        "organisation-vocabulary",
        "bulk-tags",
        "bulk-remove",
        "saved-searches",
    ):
        command += ["--bin", "organisation-query-benchmark", "--", "seed"]
    else:
        command += ["--", "seed"]
    return command + [
        "--database",
        str(database),
        "--output",
        str(output),
        "--books",
        str(workload["books"]),
        "--seed",
        str(workload["seed"]),
        "--cover-every",
        str(workload["cover_every"]),
    ]


def workload_command(
    mode: str,
    database: pathlib.Path,
    output: pathlib.Path,
    workload: dict[str, Any],
) -> list[str]:
    command = ["cargo", "run", "--release", "--locked", "-p", "lectern-benchmark"]
    if mode == "organisation-migration":
        command += ["--bin", "organisation-benchmark", "--", "migration"]
    elif mode == "organisation-query":
        command += ["--bin", "organisation-query-benchmark", "--", "query"]
    elif mode == "organisation-vocabulary":
        command += ["--bin", "organisation-vocabulary-benchmark", "--"]
    elif mode in ("bulk-tags", "bulk-remove"):
        command += ["--bin", "organisation-bulk-benchmark", "--"]
    elif mode == "saved-searches":
        command += ["--bin", "organisation-saved-search-benchmark", "--"]
    else:
        command += [
            "--",
            {
                "full": "query",
                "page": "query-page",
                "page-covered": "query-page-covered",
                "remove": "remove",
                "detach": "detach",
                "attach": "attach",
                "replace": "replace",
                "export": "export",
                "reimport": "reimport",
                "maintenance": "maintenance",
            }[mode],
        ]
    if mode in ("bulk-tags", "bulk-remove", "saved-searches"):
        command += [
            "--database",
            str(database),
            "--output",
            str(output),
            "--books",
            str(workload["books"]),
            "--iterations",
            str(workload["measured_iterations"]),
            "--warmup",
            str(workload["warmup_iterations"]),
        ]
        if mode == "bulk-remove":
            command += ["--operation", "remove"]
        return command
    return command + [
        "--database",
        str(database),
        "--output",
        str(output),
        "--books",
        str(workload["books"]),
        "--seed",
        str(workload["seed"]),
        "--cover-every",
        str(workload["cover_every"]),
        "--iterations",
        str(workload["measured_iterations"]),
        "--warmup",
        str(workload["warmup_iterations"]),
    ] if mode in ("organisation-migration", "organisation-query") else command + [
        "--database",
        str(database),
        "--output",
        str(output),
        "--iterations",
        str(workload["measured_iterations"]),
        "--warmup",
        str(workload["warmup_iterations"]),
    ]


def load_budget(path: pathlib.Path) -> dict[str, Any]:
    budget = read_json(path)
    return validate_budget(budget)


def validate_budget(budget: dict[str, Any]) -> dict[str, Any]:
    if budget.get("schema_version") != 1:
        raise RegressionError("budget.schema_version must be 1")
    if budget.get("kind") not in (
        CONFIGURATION_KIND,
        ORGANISATION_CONFIGURATION_KIND,
        UI_CONFIGURATION_KIND,
    ):
        raise RegressionError(
            "budget.kind must identify a supported query or organisation workload"
        )

    workload = object_field(budget, "workload", "budget")
    for field in ("books", "seed", "cover_every", "warmup_iterations", "measured_iterations"):
        positive_or_zero_field(workload, field, "budget.workload")
    query_mode = workload.get("query_mode", "full")
    if query_mode == "ui-bootstrap":
        if any(workload[field] != 0 for field in ("books", "seed", "cover_every")):
            raise RegressionError(
                "UI bootstrap workload books, seed, and cover_every must be zero"
            )
    elif workload["books"] == 0:
        raise RegressionError("budget.workload.books must be greater than zero")
    if workload["measured_iterations"] == 0:
        raise RegressionError(
            "budget.workload.measured_iterations must be greater than zero"
        )
    if query_mode not in (
        "full",
        "page",
        "page-covered",
        "remove",
        "detach",
        "attach",
        "replace",
        "export",
        "reimport",
        "organisation-migration",
        "organisation-query",
        "organisation-vocabulary",
        "bulk-tags",
        "bulk-remove",
        "saved-searches",
        "maintenance",
        "ui-bootstrap",
        "ui-selection",
        "ui-book-detail",
    ):
        raise RegressionError(
            "budget.workload.query_mode must be 'full', 'page', 'page-covered', "
            "'remove', 'detach', 'attach', 'replace', 'export', 'reimport', "
            "'organisation-migration', 'organisation-query', "
            "'organisation-vocabulary', 'bulk-tags', 'bulk-remove', 'saved-searches', 'maintenance', "
            "'ui-bootstrap', 'ui-selection', or 'ui-book-detail'"
        )
    if (query_mode in ("ui-bootstrap", "ui-selection", "ui-book-detail")) != (
        budget["kind"] == UI_CONFIGURATION_KIND
    ):
        raise RegressionError(
            "UI query modes and lectern-ui-regression-budget kind must be used together"
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
        if query_mode in ("remove", "detach", "attach", "replace"):
            positive_or_zero_field(workload, "page_size", "budget.workload")
            if workload["page_size"] == 0:
                raise RegressionError("budget.workload.page_size must be greater than zero")
        if query_mode in ("attach", "replace"):
            positive_or_zero_field(workload, "source_payload_bytes", "budget.workload")
            if workload["source_payload_bytes"] == 0:
                raise RegressionError(
                    "budget.workload.source_payload_bytes must be greater than zero"
                )
        if query_mode == "export":
            positive_or_zero_field(workload, "source_bytes", "budget.workload")
            positive_or_zero_field(workload, "copy_buffer_bytes", "budget.workload")
            if workload["source_bytes"] == 0 or workload["copy_buffer_bytes"] == 0:
                raise RegressionError(
                    "budget.workload export sizes must be greater than zero"
                )
        if query_mode == "organisation-migration":
            if positive_or_zero_field(
                workload, "source_schema_version", "budget.workload"
            ) != 5:
                raise RegressionError(
                    "organisation migration must use source schema version five"
                )
        if query_mode in (
            "organisation-query",
            "organisation-vocabulary",
            "bulk-tags",
            "bulk-remove",
            "saved-searches",
        ) and positive_or_zero_field(
            workload, "fixture_version", "budget.workload"
        ) != 2:
            raise RegressionError(
                "normalized organisation workload must use fixture version two"
            )
        if query_mode in ("organisation-query", "organisation-vocabulary"):
            for field in (
                "page_size",
                "contributors",
                "series",
                "tags",
                "tags_per_book",
                "saved_searches",
                "autocomplete_limit",
            ):
                if positive_or_zero_field(workload, field, "budget.workload") == 0:
                    raise RegressionError(
                        f"organisation workload {field} must be greater than zero"
                    )
        if query_mode == "organisation-vocabulary":
            if positive_or_zero_field(
                workload, "matching_books", "budget.workload"
            ) == 0:
                raise RegressionError(
                    "organisation vocabulary matching_books must be greater than zero"
                )
        if query_mode == "bulk-tags":
            for field in (
                "page_size",
                "contributors",
                "series",
                "tags",
                "tags_per_book",
                "saved_searches",
                "matching_books",
                "tags_added",
                "tags_removed",
            ):
                if positive_or_zero_field(workload, field, "budget.workload") == 0:
                    raise RegressionError(
                        f"bulk-tag workload {field} must be greater than zero"
                    )
            compositor_samples = positive_or_zero_field(
                workload, "compositor_samples", "budget.workload"
            )
            if compositor_samples == 0:
                raise RegressionError(
                    "bulk-tag workload compositor_samples must be greater than zero"
                )
            if (workload["warmup_iterations"] + compositor_samples) % 2 != 0:
                raise RegressionError(
                    "bulk-tag compositor warmup and measured operations must total an even number"
                )
        if query_mode == "bulk-remove":
            for field in ("page_size", "matching_books"):
                if positive_or_zero_field(workload, field, "budget.workload") == 0:
                    raise RegressionError(
                        f"bulk removal workload {field} must be greater than zero"
                    )
        if query_mode in ("ui-selection", "ui-book-detail"):
            if positive_or_zero_field(workload, "page_size", "budget.workload") == 0:
                raise RegressionError(
                    "populated UI workload page_size must be greater than zero"
                )
        if query_mode == "saved-searches":
            for field in (
                "contributors",
                "series",
                "tags",
                "tags_per_book",
                "saved_searches",
                "manager_page_size",
                "query_page_size",
            ):
                if positive_or_zero_field(workload, field, "budget.workload") == 0:
                    raise RegressionError(
                        f"saved-search workload {field} must be greater than zero"
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
        if query_mode in ("ui-bootstrap", "ui-selection", "ui-book-detail"):
            positive_or_zero_field(
                scenario_budget,
                "max_peak_rss_bytes",
                f"budget {name!r}",
            )
        if query_mode == "export":
            positive_number_field(
                scenario_budget,
                "min_p05_throughput_mib_per_second",
                f"budget {name!r}",
            )
            positive_or_zero_field(
                scenario_budget,
                "max_peak_rss_delta_bytes",
                f"budget {name!r}",
            )
        if query_mode == "organisation-migration":
            positive_or_zero_field(
                scenario_budget,
                "max_peak_rss_bytes",
                f"budget {name!r}",
            )
        if query_mode == "organisation-vocabulary":
            positive_or_zero_field(
                scenario_budget,
                "max_peak_rss_delta_bytes",
                f"budget {name!r}",
            )
        if query_mode == "bulk-tags" and name == "bulk_tag_apply_and_refresh":
            positive_or_zero_field(
                scenario_budget,
                "max_peak_rss_delta_bytes",
                f"budget {name!r}",
            )
        if query_mode == "bulk-remove" and name == "bulk_remove_and_refresh":
            positive_or_zero_field(
                scenario_budget,
                "max_peak_rss_delta_bytes",
                f"budget {name!r}",
            )
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


def evaluate_maintenance_result(
    result: dict[str, Any], budget: dict[str, Any]
) -> list[dict[str, Any]]:
    workload = budget["workload"]
    context = "maintenance result"
    books = workload["books"]
    if positive_or_zero_field(result, "library_books", context) != books:
        raise RegressionError("maintenance library count does not match the budget")
    if positive_or_zero_field(result, "library_assets", context) != books:
        raise RegressionError("maintenance seeded asset count does not reconcile")
    warmup = positive_or_zero_field(result, "warmup_iterations", context)
    measured = positive_or_zero_field(result, "measured_iterations", context)
    if warmup != workload["warmup_iterations"] or measured != workload["measured_iterations"]:
        raise RegressionError("maintenance iteration counts do not match the budget")
    minimum_bytes = positive_or_zero_field(result, "minimum_backup_bytes", context)
    maximum_bytes = positive_or_zero_field(result, "maximum_backup_bytes", context)
    if minimum_bytes == 0 or maximum_bytes < minimum_bytes:
        raise RegressionError("maintenance backup sizes do not reconcile")
    if positive_or_zero_field(result, "referenced_files_checked", context) != books:
        raise RegressionError("maintenance referenced-file count does not reconcile")
    expected_checks = {
        "sqlite_integrity",
        "foreign_keys",
        "fts_consistency",
        "asset_relationships",
        "referenced_file_partition",
        "backup_snapshot_count",
        "backup_snapshot_integrity",
        "backup_collision_preserved",
    }
    checks = result.get("verified_checks")
    if not isinstance(checks, list) or set(checks) != expected_checks:
        raise RegressionError("maintenance correctness checks did not reconcile")

    scenarios = result.get("scenarios")
    if not isinstance(scenarios, list) or not scenarios:
        raise RegressionError("maintenance result must contain scenarios")
    by_name: dict[str, dict[str, Any]] = {}
    for index, scenario in enumerate(scenarios):
        scenario_context = f"maintenance scenario {index}"
        if not isinstance(scenario, dict):
            raise RegressionError(f"{scenario_context} must be an object")
        name = scenario.get("name")
        if not isinstance(name, str) or not name or name in by_name:
            raise RegressionError(f"{scenario_context}.name must be unique and non-empty")
        operations = positive_or_zero_field(
            scenario, "successful_operations", scenario_context
        )
        if operations != warmup + measured:
            raise RegressionError(f"{scenario_context} operation count does not reconcile")
        samples = scenario.get("samples_ns")
        if not isinstance(samples, list) or len(samples) != measured:
            raise RegressionError(f"{scenario_context} sample count does not match the budget")
        if any(
            isinstance(sample, bool) or not isinstance(sample, int) or sample <= 0
            for sample in samples
        ):
            raise RegressionError(f"{scenario_context}.samples_ns must contain positive integers")
        latency = object_field(scenario, "latency_ms", scenario_context)
        positive_number_field(latency, "p95", f"{scenario_context}.latency_ms")
        by_name[name] = scenario

    expected_names = set(workload["scenarios"])
    if set(by_name) != expected_names or expected_names != set(budget["budgets"]):
        raise RegressionError("maintenance scenarios do not match the versioned budget")
    return evaluate_latency_budgets(by_name, budget)


def evaluate_replace_result(
    result: dict[str, Any], budget: dict[str, Any]
) -> list[dict[str, Any]]:
    workload = budget["workload"]
    books = workload["books"]
    if positive_or_zero_field(result, "library_books", "replace result") != books:
        raise RegressionError("replace workload initial library count does not match the budget")
    if positive_or_zero_field(result, "final_library_books", "replace result") != books:
        raise RegressionError("replace workload final library count does not reconcile")
    warmup = positive_or_zero_field(result, "warmup_iterations", "replace result")
    measured = positive_or_zero_field(result, "measured_iterations", "replace result")
    if warmup != workload["warmup_iterations"] or measured != workload["measured_iterations"]:
        raise RegressionError("replace iteration counts do not match the budget")
    page_size = positive_or_zero_field(result, "page_size", "replace result")
    if page_size != workload["page_size"]:
        raise RegressionError("replace refresh page size does not match the budget")
    payload_bytes = positive_or_zero_field(result, "source_payload_bytes", "replace result")
    if payload_bytes != workload["source_payload_bytes"]:
        raise RegressionError("replace source payload does not match the budget")
    source_files = result.get("source_files")
    if (
        not isinstance(source_files, list)
        or len(source_files) != 2
        or not all(isinstance(path, str) and path for path in source_files)
    ):
        raise RegressionError("replace result must retain original and replacement paths")
    verified_checks = result.get("verified_checks")
    expected_checks = {"source_bytes", "metadata", "covers", "asset_identity"}
    if (
        not isinstance(verified_checks, list)
        or len(verified_checks) != len(expected_checks)
        or set(verified_checks) != expected_checks
    ):
        raise RegressionError("replace workload did not verify source bytes, metadata, cached covers, and asset identity")

    scenarios = result.get("scenarios")
    if not isinstance(scenarios, list) or not scenarios:
        raise RegressionError("replace result must contain scenarios")
    by_name: dict[str, dict[str, Any]] = {}
    for index, scenario in enumerate(scenarios):
        context = f"replace scenario {index}"
        if not isinstance(scenario, dict):
            raise RegressionError(f"{context} must be an object")
        name = scenario.get("name")
        if not isinstance(name, str) or not name or name in by_name:
            raise RegressionError(f"{context}.name must be unique and non-empty")
        validated = positive_or_zero_field(scenario, "validated_publications", context)
        replaced = positive_or_zero_field(scenario, "successful_replacements", context)
        if validated != warmup + measured or replaced != warmup + measured:
            raise RegressionError(f"{context} operation counts do not reconcile")
        refreshed_total = positive_or_zero_field(scenario, "refreshed_total", context)
        if refreshed_total != books + 1:
            raise RegressionError(f"{context} refreshed total does not reconcile")
        expected_page = min(refreshed_total, page_size)
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
        raise RegressionError("replace scenarios do not match the versioned budget")
    return evaluate_latency_budgets(by_name, budget)


def evaluate_export_result(
    result: dict[str, Any], budget: dict[str, Any]
) -> list[dict[str, Any]]:
    workload = budget["workload"]
    context = "export result"
    if positive_or_zero_field(result, "library_books", context) != workload["books"]:
        raise RegressionError("export workload library count does not match the budget")
    for field in ("source_bytes", "copy_buffer_bytes"):
        if positive_or_zero_field(result, field, context) != workload[field]:
            raise RegressionError(f"export {field} does not match the budget")
    warmup = positive_or_zero_field(result, "warmup_iterations", context)
    measured = positive_or_zero_field(result, "measured_iterations", context)
    if warmup != workload["warmup_iterations"] or measured != workload["measured_iterations"]:
        raise RegressionError("export iteration counts do not match the budget")
    expected_checks = {
        "exact_bytes",
        "collision_preserved",
        "missing_source_rejected",
        "temporary_cleanup",
    }
    checks = result.get("verified_checks")
    if not isinstance(checks, list) or set(checks) != expected_checks:
        raise RegressionError("export correctness checks did not reconcile")
    peak_delta = positive_or_zero_field(result, "peak_rss_delta_bytes", context)

    scenarios = result.get("scenarios")
    if not isinstance(scenarios, list) or len(scenarios) != 1:
        raise RegressionError("export result must contain one scenario")
    scenario = scenarios[0]
    if not isinstance(scenario, dict) or scenario.get("name") != "export_large_file":
        raise RegressionError("export scenario does not match the versioned workload")
    if positive_or_zero_field(scenario, "successful_exports", "export scenario") != warmup + measured:
        raise RegressionError("export operation count does not reconcile")
    for field in ("samples_ns", "copy_samples_ns"):
        samples = scenario.get(field)
        if not isinstance(samples, list) or len(samples) != measured:
            raise RegressionError(f"export scenario {field} count does not match the budget")
        if any(isinstance(sample, bool) or not isinstance(sample, int) or sample <= 0 for sample in samples):
            raise RegressionError(f"export scenario {field} must contain positive integers")
    latency = object_field(scenario, "latency_ms", "export scenario")
    positive_number_field(latency, "p95", "export scenario latency")
    throughput = object_field(scenario, "throughput_mib_per_second", "export scenario")
    p05_throughput = positive_number_field(
        throughput, "p05", "export scenario throughput"
    )

    decisions = evaluate_latency_budgets({"export_large_file": scenario}, budget)
    decision = decisions[0]
    scenario_budget = budget["budgets"]["export_large_file"]
    minimum_throughput = float(
        scenario_budget["min_p05_throughput_mib_per_second"]
    )
    maximum_peak_delta = scenario_budget["max_peak_rss_delta_bytes"]
    decision |= {
        "p05_throughput_mib_per_second": p05_throughput,
        "min_p05_throughput_mib_per_second": minimum_throughput,
        "peak_rss_delta_bytes": peak_delta,
        "max_peak_rss_delta_bytes": maximum_peak_delta,
    }
    decision["passed"] = bool(
        decision["passed"]
        and p05_throughput >= minimum_throughput
        and peak_delta <= maximum_peak_delta
    )
    return decisions


def evaluate_migration_result(
    result: dict[str, Any], budget: dict[str, Any]
) -> list[dict[str, Any]]:
    workload = budget["workload"]
    context = "organisation migration result"
    if positive_or_zero_field(result, "library_books", context) != workload["books"]:
        raise RegressionError("migration library count does not match the budget")
    if positive_or_zero_field(result, "source_schema_version", context) != 5:
        raise RegressionError("migration source schema version is not five")
    if positive_or_zero_field(result, "final_schema_version", context) != 8:
        raise RegressionError("migration did not reach schema version eight")
    warmup = positive_or_zero_field(result, "warmup_iterations", context)
    measured = positive_or_zero_field(result, "measured_iterations", context)
    if warmup != workload["warmup_iterations"] or measured != workload["measured_iterations"]:
        raise RegressionError("migration iteration counts do not match the budget")
    for field in (
        "visible_projections_preserved",
        "book_asset_cover_identities_preserved",
        "fts_equivalent",
        "initial_tags_and_saved_searches_empty",
        "schema_invariants_valid",
        "duplicate_series_numbers_repaired",
        "failed_migration_rolled_back",
    ):
        if result.get(field) is not True:
            raise RegressionError(f"migration correctness check failed: {field}")

    scenarios = result.get("scenarios")
    if not isinstance(scenarios, list) or not scenarios:
        raise RegressionError("migration result must contain scenarios")
    by_name: dict[str, dict[str, Any]] = {}
    for index, scenario in enumerate(scenarios):
        context = f"migration scenario {index}"
        if not isinstance(scenario, dict):
            raise RegressionError(f"{context} must be an object")
        name = scenario.get("name")
        if not isinstance(name, str) or not name or name in by_name:
            raise RegressionError(f"{context}.name must be unique and non-empty")
        if positive_or_zero_field(scenario, "successful_migrations", context) != (
            warmup + measured
        ):
            raise RegressionError("successful migration count does not reconcile")
        samples = scenario.get("samples_ns")
        if not isinstance(samples, list) or len(samples) != measured:
            raise RegressionError("migration sample count does not match the budget")
        if any(
            isinstance(sample, bool) or not isinstance(sample, int) or sample <= 0
            for sample in samples
        ):
            raise RegressionError("migration samples must contain positive integers")
        latency = object_field(scenario, "latency_ms", context)
        positive_number_field(latency, "p95", f"{context} latency")
        positive_or_zero_field(scenario, "peak_rss_bytes", context)
        by_name[name] = scenario

    expected_names = set(workload["scenarios"])
    if set(by_name) != expected_names or expected_names != set(budget["budgets"]):
        raise RegressionError("migration scenarios do not match the versioned workload")
    decisions = evaluate_latency_budgets(by_name, budget)
    for decision in decisions:
        name = decision["name"]
        peak_rss = positive_or_zero_field(by_name[name], "peak_rss_bytes", name)
        maximum_rss = budget["budgets"][name]["max_peak_rss_bytes"]
        decision |= {
            "peak_rss_bytes": peak_rss,
            "max_peak_rss_bytes": maximum_rss,
        }
        decision["passed"] = bool(decision["passed"] and peak_rss <= maximum_rss)
    return decisions


def evaluate_organisation_query_result(
    result: dict[str, Any], budget: dict[str, Any]
) -> list[dict[str, Any]]:
    workload = budget["workload"]
    context = "organisation query result"
    if result.get("kind") != "organisation-query":
        raise RegressionError("organisation query result kind is invalid")
    if positive_or_zero_field(result, "library_books", context) != workload["books"]:
        raise RegressionError("organisation query library count does not match the budget")
    warmup = positive_or_zero_field(result, "warmup_iterations", context)
    measured = positive_or_zero_field(result, "measured_iterations", context)
    if warmup != workload["warmup_iterations"] or measured != workload["measured_iterations"]:
        raise RegressionError("organisation query iteration counts do not match the budget")
    if positive_or_zero_field(result, "page_size", context) != workload["page_size"]:
        raise RegressionError("organisation query page size does not match the budget")
    if positive_or_zero_field(
        result, "autocomplete_limit", context
    ) != workload["autocomplete_limit"]:
        raise RegressionError("organisation autocomplete limit does not match the budget")

    checks = result.get("verified_checks")
    if not isinstance(checks, list) or set(checks) != set(workload["correctness"]):
        raise RegressionError("organisation query correctness checks did not reconcile")
    plans = result.get("query_plans")
    required_indexes = {
        "book_contributors_contributor_role_book_idx",
        "series_memberships_series_index_book_idx",
        "series_memberships_series_number_uidx",
        "book_tags_tag_book_idx",
    }
    if not isinstance(plans, list) or {
        plan.get("required_index") for plan in plans if isinstance(plan, dict)
    } != required_indexes:
        raise RegressionError("organisation query plans did not cover every required index")
    if any(
        not isinstance(plan.get("details"), list) or not plan["details"]
        for plan in plans
        if isinstance(plan, dict)
    ):
        raise RegressionError("organisation query plan evidence is empty")

    scenarios = result.get("scenarios")
    if not isinstance(scenarios, list) or not scenarios:
        raise RegressionError("organisation query result must contain scenarios")
    by_name: dict[str, dict[str, Any]] = {}
    for index, scenario in enumerate(scenarios):
        scenario_context = f"organisation query scenario {index}"
        if not isinstance(scenario, dict):
            raise RegressionError(f"{scenario_context} must be an object")
        name = scenario.get("name")
        if not isinstance(name, str) or not name or name in by_name:
            raise RegressionError(f"{scenario_context}.name must be unique and non-empty")
        operations = positive_or_zero_field(
            scenario, "successful_operations", scenario_context
        )
        if operations != warmup + measured:
            raise RegressionError(f"{scenario_context} operation count does not reconcile")
        positive_or_zero_field(scenario, "observed_results", scenario_context)
        samples = scenario.get("samples_ns")
        if not isinstance(samples, list) or len(samples) != measured:
            raise RegressionError(f"{scenario_context} sample count does not match the budget")
        if any(
            isinstance(sample, bool) or not isinstance(sample, int) or sample <= 0
            for sample in samples
        ):
            raise RegressionError(f"{scenario_context}.samples_ns must contain positive integers")
        latency = object_field(scenario, "latency_ms", scenario_context)
        positive_number_field(latency, "p95", f"{scenario_context}.latency_ms")
        by_name[name] = scenario

    expected_names = set(workload["scenarios"])
    if set(by_name) != expected_names or expected_names != set(budget["budgets"]):
        raise RegressionError("organisation query scenarios do not match the versioned budget")
    return evaluate_latency_budgets(by_name, budget)


def evaluate_organisation_vocabulary_result(
    result: dict[str, Any], budget: dict[str, Any]
) -> list[dict[str, Any]]:
    workload = budget["workload"]
    context = "organisation vocabulary result"
    if result.get("kind") != "organisation-vocabulary":
        raise RegressionError("organisation vocabulary result kind is invalid")
    expected_fields = {
        "library_books": workload["books"],
        "matching_books": workload["matching_books"],
        "saved_searches": workload["saved_searches"],
        "warmup_iterations": workload["warmup_iterations"],
        "measured_iterations": workload["measured_iterations"],
    }
    for field, expected in expected_fields.items():
        if positive_or_zero_field(result, field, context) != expected:
            raise RegressionError(
                f"organisation vocabulary {field} does not match the budget"
            )
    if positive_or_zero_field(result, "page_size", context) != workload["page_size"]:
        raise RegressionError("organisation vocabulary page size does not match the budget")
    checks = result.get("verified_checks")
    if not isinstance(checks, list) or set(checks) != set(workload["correctness"]):
        raise RegressionError("organisation vocabulary correctness checks did not reconcile")
    peak_rss = positive_or_zero_field(result, "peak_rss_delta_bytes", context)

    scenarios = result.get("scenarios")
    if not isinstance(scenarios, list) or not scenarios:
        raise RegressionError("organisation vocabulary result must contain scenarios")
    by_name: dict[str, dict[str, Any]] = {}
    operations = workload["warmup_iterations"] + workload["measured_iterations"]
    for index, scenario in enumerate(scenarios):
        scenario_context = f"organisation vocabulary scenario {index}"
        if not isinstance(scenario, dict):
            raise RegressionError(f"{scenario_context} must be an object")
        name = scenario.get("name")
        if not isinstance(name, str) or not name or name in by_name:
            raise RegressionError(f"{scenario_context}.name must be unique and non-empty")
        if positive_or_zero_field(
            scenario, "successful_operations", scenario_context
        ) != operations:
            raise RegressionError(f"{scenario_context} operation count does not reconcile")
        expected_books = 0 if name == "manager_search_page" else workload["matching_books"]
        if positive_or_zero_field(
            scenario, "books_affected_per_operation", scenario_context
        ) != expected_books:
            raise RegressionError(f"{scenario_context} book count does not reconcile")
        expected_searches = 0 if name == "manager_search_page" else workload["saved_searches"]
        if positive_or_zero_field(
            scenario, "saved_searches_affected_per_operation", scenario_context
        ) != expected_searches:
            raise RegressionError(f"{scenario_context} saved-search count does not reconcile")
        positive_or_zero_field(scenario, "refreshed_result_count", scenario_context)
        samples = scenario.get("samples_ns")
        if not isinstance(samples, list) or len(samples) != workload["measured_iterations"]:
            raise RegressionError(f"{scenario_context} sample count does not match the budget")
        if any(
            isinstance(sample, bool) or not isinstance(sample, int) or sample <= 0
            for sample in samples
        ):
            raise RegressionError(f"{scenario_context}.samples_ns must contain positive integers")
        latency = object_field(scenario, "latency_ms", scenario_context)
        positive_number_field(latency, "p95", f"{scenario_context}.latency_ms")
        by_name[name] = scenario

    expected_names = set(workload["scenarios"])
    if set(by_name) != expected_names or expected_names != set(budget["budgets"]):
        raise RegressionError("organisation vocabulary scenarios do not match the budget")
    decisions = evaluate_latency_budgets(by_name, budget)
    for decision in decisions:
        maximum_rss = budget["budgets"][decision["name"]]["max_peak_rss_delta_bytes"]
        decision |= {
            "peak_rss_delta_bytes": peak_rss,
            "max_peak_rss_delta_bytes": maximum_rss,
        }
        decision["passed"] = bool(decision["passed"] and peak_rss <= maximum_rss)
    return decisions


def evaluate_saved_search_result(
    result: dict[str, Any], budget: dict[str, Any]
) -> list[dict[str, Any]]:
    workload = budget["workload"]
    context = "saved-search result"
    if result.get("kind") != "organisation-saved-searches":
        raise RegressionError("saved-search result kind is invalid")
    expected_fields = {
        "library_books": workload["books"],
        "saved_searches": workload["saved_searches"],
        "warmup_iterations": workload["warmup_iterations"],
        "measured_iterations": workload["measured_iterations"],
        "manager_page_size": workload["manager_page_size"],
        "query_page_size": workload["query_page_size"],
    }
    for field, expected in expected_fields.items():
        if positive_or_zero_field(result, field, context) != expected:
            raise RegressionError(f"saved-search {field} does not match the budget")
    checks = result.get("verified_checks")
    if not isinstance(checks, list) or set(checks) != set(workload["correctness"]):
        raise RegressionError("saved-search correctness checks did not reconcile")

    scenarios = result.get("scenarios")
    if not isinstance(scenarios, list) or not scenarios:
        raise RegressionError("saved-search result must contain scenarios")
    by_name: dict[str, dict[str, Any]] = {}
    operations = workload["warmup_iterations"] + workload["measured_iterations"]
    expected_results = {
        "bounded_saved_search_manager_page": workload["manager_page_size"],
        "saved_search_apply_first_page": workload["query_page_size"],
        "saved_search_management_cycle": 1,
    }
    for index, scenario in enumerate(scenarios):
        scenario_context = f"saved-search scenario {index}"
        if not isinstance(scenario, dict):
            raise RegressionError(f"{scenario_context} must be an object")
        name = scenario.get("name")
        if not isinstance(name, str) or not name or name in by_name:
            raise RegressionError(f"{scenario_context}.name must be unique and non-empty")
        if name not in expected_results:
            raise RegressionError(f"{scenario_context}.name is not versioned by the workload")
        if positive_or_zero_field(
            scenario, "successful_operations", scenario_context
        ) != operations:
            raise RegressionError(f"{scenario_context} operation count does not reconcile")
        if positive_or_zero_field(
            scenario, "observed_results", scenario_context
        ) != expected_results[name]:
            raise RegressionError(f"{scenario_context} result count does not reconcile")
        samples = scenario.get("samples_ns")
        if not isinstance(samples, list) or len(samples) != workload["measured_iterations"]:
            raise RegressionError(f"{scenario_context} sample count does not match the budget")
        if any(
            isinstance(sample, bool) or not isinstance(sample, int) or sample <= 0
            for sample in samples
        ):
            raise RegressionError(f"{scenario_context}.samples_ns must contain positive integers")
        latency = object_field(scenario, "latency_ms", scenario_context)
        positive_number_field(latency, "p95", f"{scenario_context}.latency_ms")
        by_name[name] = scenario

    expected_names = set(workload["scenarios"])
    if set(by_name) != expected_names or expected_names != set(budget["budgets"]):
        raise RegressionError("saved-search scenarios do not match the versioned budget")
    return evaluate_latency_budgets(by_name, budget)


def evaluate_bulk_tag_result(
    result: dict[str, Any], budget: dict[str, Any]
) -> list[dict[str, Any]]:
    workload = budget["workload"]
    context = "bulk-tag result"
    if result.get("kind") != "organisation-bulk-tags":
        raise RegressionError("bulk-tag result kind is invalid")
    expected_fields = {
        "library_books": workload["books"],
        "matching_books": workload["matching_books"],
        "warmup_iterations": workload["warmup_iterations"],
        "measured_iterations": workload["measured_iterations"],
        "page_size": workload["page_size"],
    }
    for field, expected in expected_fields.items():
        if positive_or_zero_field(result, field, context) != expected:
            raise RegressionError(f"bulk-tag {field} does not match the budget")
    checks = result.get("verified_checks")
    if not isinstance(checks, list) or set(checks) != set(workload["correctness"]):
        raise RegressionError("bulk-tag correctness checks did not reconcile")
    if positive_or_zero_field(result, "selection_materialized_summaries", context) != 0:
        raise RegressionError("bulk-tag selection materialized book summaries")
    peak_rss = positive_or_zero_field(result, "peak_rss_delta_bytes", context)

    scenarios = result.get("scenarios")
    if not isinstance(scenarios, list) or len(scenarios) != 1:
        raise RegressionError("bulk-tag result must contain one storage scenario")
    scenario = scenarios[0]
    if not isinstance(scenario, dict) or scenario.get("name") != "bulk_tag_apply_and_refresh":
        raise RegressionError("bulk-tag storage scenario is invalid")
    operations = workload["warmup_iterations"] + workload["measured_iterations"]
    exact_counts = {
        "successful_operations": operations,
        "books_matched_per_operation": workload["matching_books"],
        "relationships_added_per_operation": (
            workload["matching_books"] * workload["tags_added"]
        ),
        "relationships_removed_per_operation": (
            workload["matching_books"] * workload["tags_removed"]
        ),
        "tags_created_per_operation": workload["tags_added"],
        "refreshed_result_count": workload["page_size"],
    }
    for field, expected in exact_counts.items():
        if positive_or_zero_field(scenario, field, "bulk-tag scenario") != expected:
            raise RegressionError(f"bulk-tag scenario {field} did not reconcile")
    samples = scenario.get("samples_ns")
    if not isinstance(samples, list) or len(samples) != workload["measured_iterations"]:
        raise RegressionError("bulk-tag storage sample count does not match the budget")
    if any(
        isinstance(sample, bool) or not isinstance(sample, int) or sample <= 0
        for sample in samples
    ):
        raise RegressionError("bulk-tag storage samples must be positive integers")
    latency = object_field(scenario, "latency_ms", "bulk-tag scenario")
    p95_ms = positive_number_field(latency, "p95", "bulk-tag scenario latency")
    scenario_budget = budget["budgets"]["bulk_tag_apply_and_refresh"]
    maximum_ms = float(scenario_budget["max_p95_ms"])
    maximum_rss = scenario_budget["max_peak_rss_delta_bytes"]
    return [
        {
            "name": "bulk_tag_apply_and_refresh",
            "p95_ms": p95_ms,
            "max_p95_ms": maximum_ms,
            "peak_rss_delta_bytes": peak_rss,
            "max_peak_rss_delta_bytes": maximum_rss,
            "passed": p95_ms <= maximum_ms and peak_rss <= maximum_rss,
        }
    ]


def evaluate_bulk_remove_result(
    result: dict[str, Any], budget: dict[str, Any]
) -> list[dict[str, Any]]:
    workload = budget["workload"]
    context = "bulk removal result"
    if result.get("kind") != "organisation-bulk-remove":
        raise RegressionError("bulk removal result kind is invalid")
    expected_fields = {
        "library_books": workload["books"],
        "matching_books": workload["matching_books"],
        "final_library_books": workload["books"] - workload["matching_books"],
        "warmup_iterations": workload["warmup_iterations"],
        "measured_iterations": workload["measured_iterations"],
        "page_size": workload["page_size"],
    }
    for field, expected in expected_fields.items():
        if positive_or_zero_field(result, field, context) != expected:
            raise RegressionError(f"bulk removal {field} does not match the budget")
    checks = result.get("verified_checks")
    if not isinstance(checks, list) or set(checks) != set(workload["correctness"]):
        raise RegressionError("bulk removal correctness checks did not reconcile")
    if positive_or_zero_field(result, "selection_materialized_summaries", context) != 0:
        raise RegressionError("bulk removal selection materialized book summaries")
    if result.get("source_bytes_unchanged") is not True:
        raise RegressionError("bulk removal changed publication source bytes")
    source_file = result.get("source_file")
    if not isinstance(source_file, str) or not source_file:
        raise RegressionError("bulk removal source-file evidence is missing")
    peak_rss = positive_or_zero_field(result, "peak_rss_delta_bytes", context)

    scenarios = result.get("scenarios")
    if not isinstance(scenarios, list) or len(scenarios) != 1:
        raise RegressionError("bulk removal result must contain one storage scenario")
    scenario = scenarios[0]
    if not isinstance(scenario, dict) or scenario.get("name") != "bulk_remove_and_refresh":
        raise RegressionError("bulk removal storage scenario is invalid")
    operations = workload["warmup_iterations"] + workload["measured_iterations"]
    exact_counts = {
        "successful_operations": operations,
        "books_removed_per_operation": workload["matching_books"],
        "refreshed_library_books": workload["books"] - workload["matching_books"],
        "refreshed_result_count": workload["page_size"],
    }
    for field, expected in exact_counts.items():
        if positive_or_zero_field(scenario, field, "bulk removal scenario") != expected:
            raise RegressionError(f"bulk removal scenario {field} did not reconcile")
    for field in ("samples_ns", "removal_samples_ns", "refresh_samples_ns"):
        samples = scenario.get(field)
        if not isinstance(samples, list) or len(samples) != workload["measured_iterations"]:
            raise RegressionError(f"bulk removal {field} count does not match the budget")
        if any(
            isinstance(sample, bool) or not isinstance(sample, int) or sample <= 0
            for sample in samples
        ):
            raise RegressionError(f"bulk removal {field} must contain positive integers")
    latency = object_field(scenario, "latency_ms", "bulk removal scenario")
    p95_ms = positive_number_field(latency, "p95", "bulk removal scenario latency")
    for field in ("removal_latency_ms", "refresh_latency_ms"):
        component = object_field(scenario, field, "bulk removal scenario")
        positive_number_field(component, "p95", f"bulk removal scenario {field}")
    scenario_budget = budget["budgets"]["bulk_remove_and_refresh"]
    maximum_ms = float(scenario_budget["max_p95_ms"])
    maximum_rss = scenario_budget["max_peak_rss_delta_bytes"]
    return [
        {
            "name": "bulk_remove_and_refresh",
            "p95_ms": p95_ms,
            "max_p95_ms": maximum_ms,
            "peak_rss_delta_bytes": peak_rss,
            "max_peak_rss_delta_bytes": maximum_rss,
            "passed": p95_ms <= maximum_ms and peak_rss <= maximum_rss,
        }
    ]


def run_ui_bootstrap(
    output: pathlib.Path,
    artifact_directory: pathlib.Path,
    workload: dict[str, Any],
    commands: list[dict[str, Any]],
) -> dict[str, Any]:
    """Measure GPUI startup and the Add-books ready-to-busy paint transition."""

    run_command(
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
        timeout_seconds=1_800,
    )
    target_directory = pathlib.Path(
        os.environ.get("CARGO_TARGET_DIR", str(REPOSITORY / "target"))
    )
    if not target_directory.is_absolute():
        target_directory = REPOSITORY / target_directory
    executable = target_directory / "release/lectern-gpui"
    if sys.platform == "win32":
        executable = executable.with_suffix(".exe")
    if not executable.is_file():
        raise RegressionError(f"release GPUI executable is missing: {executable}")

    warmup = workload["warmup_iterations"]
    measured = workload["measured_iterations"]
    samples_directory = artifact_directory / "ui-samples"
    samples_directory.mkdir()
    measured_samples: list[dict[str, Any]] = []
    raw_paths: list[str] = []
    for index in range(warmup + measured):
        phase = "warmup" if index < warmup else "measured"
        phase_index = index if phase == "warmup" else index - warmup
        sample_path = samples_directory / f"{phase}-{phase_index:03d}.json"
        environment = os.environ.copy()
        environment["LECTERN_GPUI_BENCHMARK_OUTPUT"] = str(sample_path)
        run_command(
            [str(executable)],
            commands,
            environment=environment,
            timeout_seconds=15,
        )
        sample = read_json(sample_path)
        validate_ui_bootstrap_sample(sample, sample_path)
        raw_paths.append(str(sample_path))
        if phase == "measured":
            measured_samples.append(sample)

    initial_ns = [
        round(float(sample["initial_render_ms"]) * 1_000_000)
        for sample in measured_samples
    ]
    busy_ns = [
        round(float(sample["click_to_busy_paint_ms"]) * 1_000_000)
        for sample in measured_samples
    ]
    peak_samples = [
        sample["peak_rss_bytes"]
        for sample in measured_samples
        if sample.get("peak_rss_bytes") is not None
    ]
    peak_rss = max(peak_samples) if peak_samples else None
    result = {
        "kind": "lectern-ui-bootstrap-performance",
        "library_books": 0,
        "warmup_iterations": warmup,
        "measured_iterations": measured,
        "raw_samples": raw_paths,
        "correctness": measured_samples[0]["correctness"],
        "scenarios": [
            ui_scenario("initial_render", initial_ns, peak_rss),
            ui_scenario("click_to_painted_busy_state", busy_ns, peak_rss),
        ],
    }
    write_json(output, result)
    return result


def run_ui_selection(
    output: pathlib.Path,
    artifact_directory: pathlib.Path,
    workload: dict[str, Any],
    commands: list[dict[str, Any]],
) -> dict[str, Any]:
    """Measure a populated GPUI grid through selection and destructive confirmation paints."""

    run_command(
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
        timeout_seconds=1_800,
    )
    target_directory = pathlib.Path(
        os.environ.get("CARGO_TARGET_DIR", str(REPOSITORY / "target"))
    )
    if not target_directory.is_absolute():
        target_directory = REPOSITORY / target_directory
    executable = target_directory / "release/lectern-gpui"
    if sys.platform == "win32":
        executable = executable.with_suffix(".exe")
    if not executable.is_file():
        raise RegressionError(f"release GPUI executable is missing: {executable}")

    warmup = workload["warmup_iterations"]
    measured = workload["measured_iterations"]
    samples_directory = artifact_directory / "ui-samples"
    samples_directory.mkdir()
    measured_samples: list[dict[str, Any]] = []
    raw_paths: list[str] = []
    for index in range(warmup + measured):
        phase = "warmup" if index < warmup else "measured"
        phase_index = index if phase == "warmup" else index - warmup
        sample_path = samples_directory / f"{phase}-{phase_index:03d}.json"
        environment = os.environ.copy()
        environment.update(
            {
                "LECTERN_GPUI_BENCHMARK_OUTPUT": str(sample_path),
                "LECTERN_GPUI_BENCHMARK_WORKLOAD": "library-selection",
            }
        )
        run_command(
            [str(executable)],
            commands,
            environment=environment,
            timeout_seconds=15,
        )
        sample = read_json(sample_path)
        validate_ui_selection_sample(sample, sample_path, workload)
        raw_paths.append(str(sample_path))
        if phase == "measured":
            measured_samples.append(sample)

    peak_samples = [
        sample["peak_rss_bytes"]
        for sample in measured_samples
        if sample.get("peak_rss_bytes") is not None
    ]
    peak_rss = max(peak_samples) if peak_samples else None
    scenarios = []
    for name, field in (
        ("initial_library_render", "initial_render_ms"),
        ("selection_to_painted_state", "selection_to_paint_ms"),
        ("confirmation_to_painted_state", "confirmation_to_paint_ms"),
    ):
        samples_ns = [
            round(float(sample[field]) * 1_000_000) for sample in measured_samples
        ]
        scenarios.append(ui_scenario(name, samples_ns, peak_rss))
    result = {
        "kind": "lectern-ui-selection-performance",
        "library_books": workload["books"],
        "warmup_iterations": warmup,
        "measured_iterations": measured,
        "raw_samples": raw_paths,
        "correctness": measured_samples[0]["correctness"],
        "scenarios": scenarios,
    }
    write_json(output, result)
    return result


def run_ui_book_detail(
    output: pathlib.Path,
    artifact_directory: pathlib.Path,
    workload: dict[str, Any],
    commands: list[dict[str, Any]],
) -> dict[str, Any]:
    """Measure a populated GPUI grid through opening one complete book-detail panel."""

    run_command(
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
        timeout_seconds=1_800,
    )
    target_directory = pathlib.Path(
        os.environ.get("CARGO_TARGET_DIR", str(REPOSITORY / "target"))
    )
    if not target_directory.is_absolute():
        target_directory = REPOSITORY / target_directory
    executable = target_directory / "release/lectern-gpui"
    if sys.platform == "win32":
        executable = executable.with_suffix(".exe")
    if not executable.is_file():
        raise RegressionError(f"release GPUI executable is missing: {executable}")

    warmup = workload["warmup_iterations"]
    measured = workload["measured_iterations"]
    samples_directory = artifact_directory / "ui-samples"
    samples_directory.mkdir()
    measured_samples: list[dict[str, Any]] = []
    raw_paths: list[str] = []
    for index in range(warmup + measured):
        phase = "warmup" if index < warmup else "measured"
        phase_index = index if phase == "warmup" else index - warmup
        sample_path = samples_directory / f"{phase}-{phase_index:03d}.json"
        environment = os.environ.copy()
        environment.update(
            {
                "LECTERN_GPUI_BENCHMARK_OUTPUT": str(sample_path),
                "LECTERN_GPUI_BENCHMARK_WORKLOAD": "book-detail",
            }
        )
        run_command(
            [str(executable)],
            commands,
            environment=environment,
            timeout_seconds=15,
        )
        sample = read_json(sample_path)
        validate_ui_book_detail_sample(sample, sample_path, workload)
        raw_paths.append(str(sample_path))
        if phase == "measured":
            measured_samples.append(sample)

    peak_samples = [
        sample["peak_rss_bytes"]
        for sample in measured_samples
        if sample.get("peak_rss_bytes") is not None
    ]
    peak_rss = max(peak_samples) if peak_samples else None
    scenarios = []
    for name, field in (
        ("initial_library_render", "initial_render_ms"),
        ("book_detail_to_painted_state", "detail_to_paint_ms"),
    ):
        samples_ns = [
            round(float(sample[field]) * 1_000_000) for sample in measured_samples
        ]
        scenarios.append(ui_scenario(name, samples_ns, peak_rss))
    result = {
        "kind": "lectern-ui-book-detail-performance",
        "library_books": workload["books"],
        "warmup_iterations": warmup,
        "measured_iterations": measured,
        "raw_samples": raw_paths,
        "correctness": measured_samples[0]["correctness"],
        "scenarios": scenarios,
    }
    write_json(output, result)
    return result


def validate_ui_bootstrap_sample(sample: dict[str, Any], path: pathlib.Path) -> None:
    context = f"UI sample {path.name}"
    if sample.get("schema_version") != 1:
        raise RegressionError(f"{context} schema_version must be 1")
    if sample.get("workload") != "empty-library-add-books":
        raise RegressionError(f"{context} workload identity is invalid")
    positive_number_field(sample, "initial_render_ms", context)
    positive_number_field(sample, "click_to_busy_paint_ms", context)
    peak_rss = sample.get("peak_rss_bytes")
    if peak_rss is not None and positive_or_zero_field(sample, "peak_rss_bytes", context) == 0:
        raise RegressionError(f"{context} peak_rss_bytes must be positive when present")
    if sample.get("correctness") != expected_ui_correctness():
        raise RegressionError(f"{context} correctness markers are invalid")


def expected_ui_correctness() -> dict[str, Any]:
    return {
        "heading": "Your library is empty",
        "explanation": "Add EPUB or PDF files to start building your library.",
        "ready_button_label": "Add books",
        "busy_button_label": "Adding books…",
        "initial_state_presented": True,
        "busy_state_presented": True,
    }


def validate_ui_selection_sample(
    sample: dict[str, Any], path: pathlib.Path, workload: dict[str, Any]
) -> None:
    context = f"UI selection sample {path.name}"
    if sample.get("schema_version") != 1:
        raise RegressionError(f"{context} schema_version must be 1")
    if sample.get("workload") != "library-selection":
        raise RegressionError(f"{context} workload identity is invalid")
    for field in (
        "initial_render_ms",
        "selection_to_paint_ms",
        "confirmation_to_paint_ms",
    ):
        positive_number_field(sample, field, context)
    peak_rss = sample.get("peak_rss_bytes")
    if peak_rss is not None and positive_or_zero_field(
        sample, "peak_rss_bytes", context
    ) == 0:
        raise RegressionError(f"{context} peak_rss_bytes must be positive when present")
    if sample.get("correctness") != expected_ui_selection_correctness(workload):
        raise RegressionError(f"{context} correctness markers are invalid")


def expected_ui_selection_correctness(workload: dict[str, Any]) -> dict[str, Any]:
    return {
        "library_total": workload["books"],
        "rendered_books": workload["page_size"],
        "selected_books": 1,
        "markers": [
            "compact_explicit_selection",
            "selection_bar_presented",
            "confirmation_presented",
            "removal_copy_mentions_files_remain",
        ],
    }


def validate_ui_book_detail_sample(
    sample: dict[str, Any], path: pathlib.Path, workload: dict[str, Any]
) -> None:
    context = f"UI book-detail sample {path.name}"
    if sample.get("schema_version") != 1:
        raise RegressionError(f"{context} schema_version must be 1")
    if sample.get("workload") != "book-detail":
        raise RegressionError(f"{context} workload identity is invalid")
    for field in ("initial_render_ms", "detail_to_paint_ms"):
        positive_number_field(sample, field, context)
    peak_rss = sample.get("peak_rss_bytes")
    if peak_rss is not None and positive_or_zero_field(
        sample, "peak_rss_bytes", context
    ) == 0:
        raise RegressionError(f"{context} peak_rss_bytes must be positive when present")
    if sample.get("correctness") != expected_ui_book_detail_correctness(workload):
        raise RegressionError(f"{context} correctness markers are invalid")


def expected_ui_book_detail_correctness(workload: dict[str, Any]) -> dict[str, Any]:
    return {
        "library_total": workload["books"],
        "rendered_books": workload["page_size"],
        "title": "Benchmark book 001",
        "contributor_count": 3,
        "tag_count": 2,
        "asset_count": 2,
        "markers": [
            "bounded_first_page",
            "book_detail_panel_presented",
            "complete_metadata_fixture",
            "multiple_assets_presented",
        ],
    }


def ui_scenario(name: str, samples_ns: list[int], peak_rss: int | None) -> dict[str, Any]:
    return {
        "name": name,
        "samples_ns": samples_ns,
        "latency_ms": {"p95": nearest_rank_p95(samples_ns) / 1_000_000},
        "peak_rss_bytes": peak_rss,
    }


def nearest_rank_p95(samples: list[int]) -> int:
    if not samples:
        raise RegressionError("p95 requires at least one retained sample")
    ordered = sorted(samples)
    return ordered[math.ceil(len(ordered) * 0.95) - 1]


def evaluate_ui_bootstrap_result(
    result: dict[str, Any], budget: dict[str, Any]
) -> list[dict[str, Any]]:
    workload = budget["workload"]
    context = "UI bootstrap result"
    if result.get("kind") != "lectern-ui-bootstrap-performance":
        raise RegressionError(f"{context} kind is invalid")
    if positive_or_zero_field(result, "library_books", context) != 0:
        raise RegressionError(f"{context} must use an empty library")
    if result.get("correctness") != expected_ui_correctness():
        raise RegressionError(f"{context} correctness markers are invalid")
    warmup = positive_or_zero_field(result, "warmup_iterations", context)
    measured = positive_or_zero_field(result, "measured_iterations", context)
    if warmup != workload["warmup_iterations"] or measured != workload["measured_iterations"]:
        raise RegressionError(f"{context} iteration counts do not match the budget")
    raw_samples = result.get("raw_samples")
    if (
        not isinstance(raw_samples, list)
        or len(raw_samples) != warmup + measured
        or not all(isinstance(path, str) and path for path in raw_samples)
    ):
        raise RegressionError(f"{context} must retain every raw sample path")
    scenarios = result.get("scenarios")
    if not isinstance(scenarios, list) or not scenarios:
        raise RegressionError(f"{context} must contain scenarios")
    by_name: dict[str, dict[str, Any]] = {}
    for index, scenario in enumerate(scenarios):
        scenario_context = f"UI scenario {index}"
        if not isinstance(scenario, dict):
            raise RegressionError(f"{scenario_context} must be an object")
        name = scenario.get("name")
        if not isinstance(name, str) or not name or name in by_name:
            raise RegressionError(f"{scenario_context}.name must be unique")
        samples = positive_samples(scenario, scenario_context, measured)
        p95_ms = positive_number_field(
            object_field(scenario, "latency_ms", scenario_context),
            "p95",
            f"{scenario_context}.latency_ms",
        )
        expected_p95_ms = nearest_rank_p95(samples) / 1_000_000
        if not math.isclose(p95_ms, expected_p95_ms, rel_tol=0.0, abs_tol=1e-9):
            raise RegressionError(f"{scenario_context} p95 does not match retained samples")
        peak_rss = scenario.get("peak_rss_bytes")
        if peak_rss is not None and positive_or_zero_field(
            scenario, "peak_rss_bytes", scenario_context
        ) == 0:
            raise RegressionError(f"{scenario_context} peak RSS must be positive")
        by_name[name] = scenario

    expected_names = set(workload["scenarios"])
    if set(by_name) != expected_names or expected_names != set(budget["budgets"]):
        raise RegressionError("UI scenarios do not match the versioned budget")
    decisions = []
    for name in sorted(expected_names):
        scenario = by_name[name]
        scenario_budget = budget["budgets"][name]
        p95_ms = float(scenario["latency_ms"]["p95"])
        maximum_ms = float(scenario_budget["max_p95_ms"])
        peak_rss = scenario.get("peak_rss_bytes")
        maximum_rss = scenario_budget["max_peak_rss_bytes"]
        memory_passed = peak_rss is None or peak_rss <= maximum_rss
        decisions.append(
            {
                "name": name,
                "p95_ms": p95_ms,
                "max_p95_ms": maximum_ms,
                "sample_count": measured,
                "peak_rss_bytes": peak_rss,
                "max_peak_rss_bytes": maximum_rss,
                "passed": p95_ms <= maximum_ms and memory_passed,
            }
        )
    return decisions


def evaluate_ui_selection_result(
    result: dict[str, Any], budget: dict[str, Any]
) -> list[dict[str, Any]]:
    workload = budget["workload"]
    context = "UI selection result"
    if result.get("kind") != "lectern-ui-selection-performance":
        raise RegressionError(f"{context} kind is invalid")
    if (
        positive_or_zero_field(result, "library_books", context)
        != workload["books"]
    ):
        raise RegressionError(f"{context} library size does not match the budget")
    if result.get("correctness") != expected_ui_selection_correctness(workload):
        raise RegressionError(f"{context} correctness markers are invalid")
    warmup = positive_or_zero_field(result, "warmup_iterations", context)
    measured = positive_or_zero_field(result, "measured_iterations", context)
    if warmup != workload["warmup_iterations"] or measured != workload["measured_iterations"]:
        raise RegressionError(f"{context} iteration counts do not match the budget")
    raw_samples = result.get("raw_samples")
    if (
        not isinstance(raw_samples, list)
        or len(raw_samples) != warmup + measured
        or not all(isinstance(path, str) and path for path in raw_samples)
    ):
        raise RegressionError(f"{context} must retain every raw sample path")
    scenarios = result.get("scenarios")
    if not isinstance(scenarios, list) or not scenarios:
        raise RegressionError(f"{context} must contain scenarios")
    by_name: dict[str, dict[str, Any]] = {}
    for index, scenario in enumerate(scenarios):
        scenario_context = f"UI selection scenario {index}"
        if not isinstance(scenario, dict):
            raise RegressionError(f"{scenario_context} must be an object")
        name = scenario.get("name")
        if not isinstance(name, str) or not name or name in by_name:
            raise RegressionError(f"{scenario_context}.name must be unique")
        samples = positive_samples(scenario, scenario_context, measured)
        p95_ms = positive_number_field(
            object_field(scenario, "latency_ms", scenario_context),
            "p95",
            f"{scenario_context}.latency_ms",
        )
        expected_p95_ms = nearest_rank_p95(samples) / 1_000_000
        if not math.isclose(p95_ms, expected_p95_ms, rel_tol=0.0, abs_tol=1e-9):
            raise RegressionError(f"{scenario_context} p95 does not match retained samples")
        peak_rss = scenario.get("peak_rss_bytes")
        if peak_rss is not None and positive_or_zero_field(
            scenario, "peak_rss_bytes", scenario_context
        ) == 0:
            raise RegressionError(f"{scenario_context} peak RSS must be positive")
        by_name[name] = scenario

    expected_names = set(workload["scenarios"])
    if set(by_name) != expected_names or expected_names != set(budget["budgets"]):
        raise RegressionError("UI selection scenarios do not match the versioned budget")
    decisions = []
    for name in sorted(expected_names):
        scenario = by_name[name]
        scenario_budget = budget["budgets"][name]
        p95_ms = float(scenario["latency_ms"]["p95"])
        maximum_ms = float(scenario_budget["max_p95_ms"])
        peak_rss = scenario.get("peak_rss_bytes")
        maximum_rss = scenario_budget["max_peak_rss_bytes"]
        memory_passed = peak_rss is None or peak_rss <= maximum_rss
        decisions.append(
            {
                "name": name,
                "p95_ms": p95_ms,
                "max_p95_ms": maximum_ms,
                "sample_count": measured,
                "peak_rss_bytes": peak_rss,
                "max_peak_rss_bytes": maximum_rss,
                "passed": p95_ms <= maximum_ms and memory_passed,
            }
        )
    return decisions


def evaluate_ui_book_detail_result(
    result: dict[str, Any], budget: dict[str, Any]
) -> list[dict[str, Any]]:
    workload = budget["workload"]
    context = "UI book-detail result"
    if result.get("kind") != "lectern-ui-book-detail-performance":
        raise RegressionError(f"{context} kind is invalid")
    if positive_or_zero_field(result, "library_books", context) != workload["books"]:
        raise RegressionError(f"{context} library size does not match the budget")
    if result.get("correctness") != expected_ui_book_detail_correctness(workload):
        raise RegressionError(f"{context} correctness markers are invalid")
    warmup = positive_or_zero_field(result, "warmup_iterations", context)
    measured = positive_or_zero_field(result, "measured_iterations", context)
    if warmup != workload["warmup_iterations"] or measured != workload["measured_iterations"]:
        raise RegressionError(f"{context} iteration counts do not match the budget")
    raw_samples = result.get("raw_samples")
    if (
        not isinstance(raw_samples, list)
        or len(raw_samples) != warmup + measured
        or not all(isinstance(path, str) and path for path in raw_samples)
    ):
        raise RegressionError(f"{context} must retain every raw sample path")
    scenarios = result.get("scenarios")
    if not isinstance(scenarios, list) or not scenarios:
        raise RegressionError(f"{context} must contain scenarios")
    by_name: dict[str, dict[str, Any]] = {}
    for index, scenario in enumerate(scenarios):
        scenario_context = f"UI book-detail scenario {index}"
        if not isinstance(scenario, dict):
            raise RegressionError(f"{scenario_context} must be an object")
        name = scenario.get("name")
        if not isinstance(name, str) or not name or name in by_name:
            raise RegressionError(f"{scenario_context}.name must be unique")
        samples = positive_samples(scenario, scenario_context, measured)
        p95_ms = positive_number_field(
            object_field(scenario, "latency_ms", scenario_context),
            "p95",
            f"{scenario_context}.latency_ms",
        )
        expected_p95_ms = nearest_rank_p95(samples) / 1_000_000
        if not math.isclose(p95_ms, expected_p95_ms, rel_tol=0.0, abs_tol=1e-9):
            raise RegressionError(f"{scenario_context} p95 does not match retained samples")
        peak_rss = scenario.get("peak_rss_bytes")
        if peak_rss is not None and positive_or_zero_field(
            scenario, "peak_rss_bytes", scenario_context
        ) == 0:
            raise RegressionError(f"{scenario_context} peak RSS must be positive")
        by_name[name] = scenario

    expected_names = set(workload["scenarios"])
    if set(by_name) != expected_names or expected_names != set(budget["budgets"]):
        raise RegressionError("UI book-detail scenarios do not match the versioned budget")
    decisions = []
    for name in sorted(expected_names):
        scenario = by_name[name]
        scenario_budget = budget["budgets"][name]
        p95_ms = float(scenario["latency_ms"]["p95"])
        maximum_ms = float(scenario_budget["max_p95_ms"])
        peak_rss = scenario.get("peak_rss_bytes")
        maximum_rss = scenario_budget["max_peak_rss_bytes"]
        memory_passed = peak_rss is None or peak_rss <= maximum_rss
        decisions.append(
            {
                "name": name,
                "p95_ms": p95_ms,
                "max_p95_ms": maximum_ms,
                "sample_count": measured,
                "peak_rss_bytes": peak_rss,
                "max_peak_rss_bytes": maximum_rss,
                "passed": p95_ms <= maximum_ms and memory_passed,
            }
        )
    return decisions


def run_bulk_tag_desktop(
    database: pathlib.Path,
    output: pathlib.Path,
    workload: dict[str, Any],
    commands: list[dict[str, Any]],
) -> None:
    if not (os.environ.get("DISPLAY") or os.environ.get("WAYLAND_DISPLAY")):
        raise RegressionError("bulk-tag compositor measurements require an active display")
    tag_ids = resolve_bulk_tag_ids(database)
    environment = os.environ.copy()
    if environment.get("DISPLAY"):
        environment.pop("WAYLAND_DISPLAY", None)
        environment["WINIT_UNIX_BACKEND"] = "x11"
    environment.update(
        {
            "LECTERN_DATA_DIR": str(database.parent),
            "LECTERN_BENCHMARK_OUTPUT": str(output),
            "LECTERN_BENCHMARK_IDLE_SECONDS": "0.5",
            "LECTERN_BENCHMARK_SCROLL_SECONDS": "0",
            "LECTERN_BENCHMARK_SCROLL_WARMUP_SECONDS": "0",
            "LECTERN_BENCHMARK_SORT_ITERATIONS": "0",
            "LECTERN_BENCHMARK_ASSET_ACTION_ITERATIONS": "0",
            "LECTERN_BENCHMARK_EDITOR_WARMUP_ITERATIONS": "0",
            "LECTERN_BENCHMARK_EDITOR_ITERATIONS": "0",
            "LECTERN_BENCHMARK_SELECTION_WARMUP_ITERATIONS": str(
                workload["warmup_iterations"]
            ),
            "LECTERN_BENCHMARK_SELECTION_ITERATIONS": str(
                workload["compositor_samples"]
            ),
            "LECTERN_BENCHMARK_BULK_WARMUP_ITERATIONS": str(
                workload["warmup_iterations"]
            ),
            "LECTERN_BENCHMARK_BULK_ITERATIONS": str(
                workload["compositor_samples"]
            ),
            "LECTERN_BENCHMARK_BULK_BASELINE_TAG_ID": str(tag_ids["Bulk baseline"]),
            "LECTERN_BENCHMARK_BULK_ADD_TAG_A_ID": str(tag_ids["Bulk added A 000"]),
            "LECTERN_BENCHMARK_BULK_ADD_TAG_B_ID": str(tag_ids["Bulk added B 000"]),
            "LECTERN_BENCHMARK_TIMEOUT_SECONDS": "240",
            "WGPU_BACKEND": environment.get("WGPU_BACKEND", "vulkan"),
        }
    )
    run_command(
        [
            "cargo",
            "run",
            "--release",
            "--locked",
            "-p",
            "lectern-desktop",
            "--bin",
            "lectern",
        ],
        commands,
        environment=environment,
        timeout_seconds=270,
    )


def resolve_bulk_tag_ids(database: pathlib.Path) -> dict[str, int]:
    names = ("Bulk baseline", "Bulk added A 000", "Bulk added B 000")
    with sqlite3.connect(database) as connection:
        rows = connection.execute(
            "SELECT name, id FROM tags WHERE name IN (?, ?, ?)", names
        ).fetchall()
    resolved = {str(name): int(identifier) for name, identifier in rows}
    if set(resolved) != set(names) or any(identifier <= 0 for identifier in resolved.values()):
        raise RegressionError("bulk-tag storage setup did not retain benchmark tag identities")
    return resolved


def evaluate_bulk_tag_desktop_result(
    result: dict[str, Any], budget: dict[str, Any]
) -> list[dict[str, Any]]:
    workload = budget["workload"]
    samples = workload["compositor_samples"]
    if result.get("kind") != "desktop" or result.get("status") != "completed":
        raise RegressionError(
            f"bulk-tag desktop benchmark failed: {result.get('error')}"
        )
    if positive_or_zero_field(result, "schema_version", "desktop result") < 6:
        raise RegressionError("bulk-tag desktop result schema is too old")
    library = object_field(result, "library", "desktop result")
    if positive_or_zero_field(library, "books", "desktop library") != workload["books"]:
        raise RegressionError("bulk-tag desktop library count did not reconcile")

    selection = object_field(result, "selection_interactions", "desktop result")
    if positive_or_zero_field(selection, "measured_iterations", "desktop selection") != samples:
        raise RegressionError("desktop selection sample configuration did not reconcile")
    if positive_or_zero_field(selection, "matching_books", "desktop selection") != workload["books"]:
        raise RegressionError("desktop selection count did not reconcile")
    selection_samples = positive_samples(selection, "desktop selection", samples)
    selection_latency = object_field(selection, "latency", "desktop selection")
    selection_p95_ns = positive_or_zero_field(
        selection_latency, "p95_ns", "desktop selection latency"
    )
    if selection_p95_ns == 0 or selection.get("passed") is not True:
        raise RegressionError("desktop selection endpoint exceeded its absolute budget")

    bulk = object_field(result, "bulk_tag_interactions", "desktop result")
    if positive_or_zero_field(bulk, "measured_iterations", "desktop bulk tags") != samples:
        raise RegressionError("desktop bulk-tag sample configuration did not reconcile")
    if positive_or_zero_field(bulk, "matching_books", "desktop bulk tags") != workload["matching_books"]:
        raise RegressionError("desktop bulk-tag target count did not reconcile")
    total_operations = workload["warmup_iterations"] + samples
    forward = positive_or_zero_field(bulk, "forward_operations", "desktop bulk tags")
    inverse = positive_or_zero_field(bulk, "inverse_operations", "desktop bulk tags")
    if forward + inverse != total_operations or abs(forward - inverse) > 1:
        raise RegressionError("desktop bulk-tag forward/inverse operations did not reconcile")
    expected_bulk_counts = {
        "forward_expected_relationships_added": (
            workload["matching_books"] * workload["tags_added"]
        ),
        "forward_expected_relationships_removed": (
            workload["matching_books"] * workload["tags_removed"]
        ),
        "inverse_expected_relationships_added": (
            workload["matching_books"] * workload["tags_removed"]
        ),
        "inverse_expected_relationships_removed": (
            workload["matching_books"] * workload["tags_added"]
        ),
    }
    for field, expected in expected_bulk_counts.items():
        if positive_or_zero_field(bulk, field, "desktop bulk tags") != expected:
            raise RegressionError(f"desktop bulk-tag {field} did not reconcile")
    bulk_samples = positive_samples(bulk, "desktop bulk tags", samples)
    bulk_latency = object_field(bulk, "latency", "desktop bulk tags")
    bulk_p95_ns = positive_or_zero_field(
        bulk_latency, "p95_ns", "desktop bulk-tag latency"
    )
    if bulk_p95_ns == 0 or bulk.get("passed") is not True:
        raise RegressionError("desktop bulk-tag endpoint exceeded its absolute budget")

    endpoints = (
        (
            "selection_dispatch_to_busy_paint",
            selection_p95_ns,
            selection_samples,
        ),
        (
            "completion_to_refreshed_grid_paint",
            bulk_p95_ns,
            bulk_samples,
        ),
    )
    decisions = []
    for name, p95_ns, retained_samples in endpoints:
        maximum_ms = float(budget["budgets"][name]["max_p95_ms"])
        p95_ms = p95_ns / 1_000_000
        decisions.append(
            {
                "name": name,
                "p95_ms": p95_ms,
                "max_p95_ms": maximum_ms,
                "sample_count": len(retained_samples),
                "passed": p95_ms <= maximum_ms,
            }
        )
    return decisions


def positive_samples(value: dict[str, Any], context: str, expected: int) -> list[int]:
    samples = value.get("samples_ns")
    if not isinstance(samples, list) or len(samples) != expected:
        raise RegressionError(f"{context} retained an invalid sample count")
    if any(
        isinstance(sample, bool) or not isinstance(sample, int) or sample <= 0
        for sample in samples
    ):
        raise RegressionError(f"{context} samples must be positive integers")
    return samples


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


def run_command(
    command: list[str],
    commands: list[dict[str, Any]],
    *,
    environment: dict[str, str] | None = None,
    timeout_seconds: float | None = None,
) -> None:
    print(f"+ {shlex.join(command)}", flush=True)
    started_at = utc_now()
    started = time.monotonic_ns()
    try:
        result = subprocess.run(
            command,
            cwd=REPOSITORY,
            env=environment,
            check=False,
            timeout=timeout_seconds,
        )
    except subprocess.TimeoutExpired as error:
        commands.append(
            {
                "command": command,
                "started_at_utc": started_at,
                "elapsed_ns": time.monotonic_ns() - started,
                "return_code": None,
                "timeout_seconds": timeout_seconds,
                "timed_out": True,
            }
        )
        raise RegressionError(
            f"command timed out after {timeout_seconds} seconds: {shlex.join(command)}"
        ) from error
    record = {
        "command": command,
        "started_at_utc": started_at,
        "elapsed_ns": time.monotonic_ns() - started,
        "return_code": result.returncode,
        "timeout_seconds": timeout_seconds,
        "timed_out": False,
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


def validate_migration_seed_result(
    result: dict[str, Any], workload: dict[str, Any]
) -> None:
    context = "organisation migration seed"
    if result.get("kind") != "organisation-migration-seed":
        raise RegressionError("migration seed kind is invalid")
    if positive_or_zero_field(result, "library_books", context) != workload["books"]:
        raise RegressionError("migration seed book count does not match the budget")
    if positive_or_zero_field(result, "contributor_vocabulary", context) != min(
        workload["books"], 20_000
    ):
        raise RegressionError("migration seed contributor distribution is invalid")
    if positive_or_zero_field(result, "series_vocabulary", context) != min(
        2_500,
        (workload["books"] // 10) * 7 + min(workload["books"] % 10, 7),
    ):
        raise RegressionError("migration seed series distribution is invalid")
    positive_or_zero_field(result, "covers", context)
    positive_or_zero_field(result, "database_bytes", context)


def validate_organisation_query_seed_result(
    result: dict[str, Any], workload: dict[str, Any]
) -> None:
    context = "organisation query seed"
    if result.get("kind") != "organisation-query-seed":
        raise RegressionError("organisation query seed kind is invalid")
    if positive_or_zero_field(result, "fixture_version", context) != workload.get(
        "fixture_version", 1
    ):
        raise RegressionError("organisation query seed fixture version is invalid")
    expected = {
        "library_books": workload["books"],
        "contributors": workload["contributors"],
        "series": workload["series"],
        "tags": workload["tags"],
        "tags_per_book": workload["tags_per_book"],
        "saved_searches": workload["saved_searches"],
    }
    for field, value in expected.items():
        if positive_or_zero_field(result, field, context) != value:
            raise RegressionError(f"organisation query seed {field} does not match the budget")
    positive_or_zero_field(result, "database_bytes", context)


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
