#!/usr/bin/env python3
"""Compare paired Lectern p95 results from a base and candidate revision."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
import pathlib
import statistics
import sys
from typing import Any


BUDGET_KIND = "lectern-query-regression-budget"
INPUT_KIND = "lectern-query-performance-regression"
RESULT_KIND = "lectern-query-performance-comparison"


class ComparisonError(RuntimeError):
    """The paired performance inputs are invalid or exceed their relative budget."""


def parse_arguments(arguments: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Compare base and candidate p95 query results on the same runner."
    )
    parser.add_argument(
        "--base-result", type=pathlib.Path, action="append", required=True
    )
    parser.add_argument(
        "--candidate-result", type=pathlib.Path, action="append", required=True
    )
    parser.add_argument("--budget", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument(
        "--github-summary",
        type=pathlib.Path,
        help="Append a Markdown comparison table to GitHub's step summary",
    )
    return parser.parse_args(arguments)


def main(arguments: list[str]) -> int:
    options = parse_arguments(arguments)
    report: dict[str, Any] = {
        "schema_version": 1,
        "kind": RESULT_KIND,
        "created_at_utc": utc_now(),
        "status": "running",
    }
    try:
        budget = read_json(options.budget)
        base = [read_json(path) for path in options.base_result]
        candidate = [read_json(path) for path in options.candidate_result]
        report |= compare_result_sets(base, candidate, budget)
        failures = [decision for decision in report["decisions"] if not decision["passed"]]
        if failures:
            failed_names = ", ".join(decision["name"] for decision in failures)
            raise ComparisonError(f"relative p95 budget exceeded: {failed_names}")
        report["status"] = "passed"
        print(f"Paired performance comparison passed: {options.output}")
        return_code = 0
    except (OSError, ValueError, ComparisonError) as error:
        report["status"] = "failed"
        report["error"] = str(error)
        print(f"error: {error}", file=sys.stderr)
        return_code = 2

    write_json(options.output, report)
    if options.github_summary is not None:
        append_github_summary(options.github_summary, report)
    return return_code


def compare_results(
    base: dict[str, Any], candidate: dict[str, Any], budget: dict[str, Any]
) -> dict[str, Any]:
    """Compare one result pair, primarily for focused callers and tests."""

    return compare_result_sets([base], [candidate], budget)


def compare_result_sets(
    base_results: list[dict[str, Any]],
    candidate_results: list[dict[str, Any]],
    budget: dict[str, Any],
) -> dict[str, Any]:
    """Validate paired result sets and evaluate median run-level p95 values."""

    validate_budget(budget)
    expected_runs = budget["comparison"]["paired_runs"]
    if len(base_results) != expected_runs or len(candidate_results) != expected_runs:
        raise ComparisonError(
            f"budget requires exactly {expected_runs} paired runs; got "
            f"base={len(base_results)}, candidate={len(candidate_results)}"
        )
    labelled_results = [
        (f"base run {index}", result)
        for index, result in enumerate(base_results, start=1)
    ]
    labelled_results += [
        (f"candidate run {index}", result)
        for index, result in enumerate(candidate_results, start=1)
    ]
    for label, result in labelled_results:
        validate_result(result, label)
        validate_same_workload(result, budget, label)
    if any(candidate.get("status") != "passed" for candidate in candidate_results):
        raise ComparisonError("every candidate run must pass its absolute performance budget")
    reference = base_results[0]
    for label, result in labelled_results[1:]:
        validate_same_environment(reference, result, label)

    base_scenario_runs = [
        scenario_p95(result, f"base run {index}")
        for index, result in enumerate(base_results, start=1)
    ]
    candidate_scenario_runs = [
        scenario_p95(result, f"candidate run {index}")
        for index, result in enumerate(candidate_results, start=1)
    ]
    expected = set(budget["budgets"])
    labelled_scenarios = [
        (f"base run {index}", scenarios)
        for index, scenarios in enumerate(base_scenario_runs, start=1)
    ]
    labelled_scenarios += [
        (f"candidate run {index}", scenarios)
        for index, scenarios in enumerate(candidate_scenario_runs, start=1)
    ]
    for label, scenarios in labelled_scenarios:
        if set(scenarios) != expected:
            raise ComparisonError(
                f"{label} scenarios do not match the versioned budget: "
                f"expected {sorted(expected)}, got {sorted(scenarios)}"
            )

    comparison = budget["comparison"]
    maximum_percent = float(comparison["max_p95_regression_percent"])
    minimum_delta_ms = float(comparison["minimum_p95_delta_ms"])
    decisions = []
    for name in sorted(expected):
        base_run_p95_ms = [scenarios[name] for scenarios in base_scenario_runs]
        candidate_run_p95_ms = [
            scenarios[name] for scenarios in candidate_scenario_runs
        ]
        base_p95_ms = statistics.median(base_run_p95_ms)
        candidate_p95_ms = statistics.median(candidate_run_p95_ms)
        delta_ms = candidate_p95_ms - base_p95_ms
        regression_percent = delta_ms / base_p95_ms * 100.0
        passed = not (
            delta_ms > minimum_delta_ms and regression_percent > maximum_percent
        )
        decisions.append(
            {
                "name": name,
                "base_run_p95_ms": base_run_p95_ms,
                "candidate_run_p95_ms": candidate_run_p95_ms,
                "base_p95_ms": base_p95_ms,
                "candidate_p95_ms": candidate_p95_ms,
                "delta_ms": delta_ms,
                "regression_percent": regression_percent,
                "max_p95_regression_percent": maximum_percent,
                "minimum_p95_delta_ms": minimum_delta_ms,
                "passed": passed,
            }
        )

    return {
        "budget": {
            "schema_version": budget["schema_version"],
            "kind": budget["kind"],
        },
        "base_runs": [result_identity(result) for result in base_results],
        "candidate_runs": [result_identity(result) for result in candidate_results],
        "decisions": decisions,
    }


def validate_budget(budget: dict[str, Any]) -> None:
    if budget.get("schema_version") != 1 or budget.get("kind") != BUDGET_KIND:
        raise ComparisonError("comparison requires a version 1 query-regression budget")
    budgets = budget.get("budgets")
    if not isinstance(budgets, dict) or not budgets:
        raise ComparisonError("budget.budgets must be a non-empty object")
    comparison = budget.get("comparison")
    if not isinstance(comparison, dict):
        raise ComparisonError("budget.comparison must be an object")
    paired_runs = comparison.get("paired_runs")
    if (
        isinstance(paired_runs, bool)
        or not isinstance(paired_runs, int)
        or paired_runs <= 0
    ):
        raise ComparisonError("budget.comparison.paired_runs must be a positive integer")
    positive_number(
        comparison.get("max_p95_regression_percent"),
        "budget.comparison.max_p95_regression_percent",
    )
    non_negative_number(
        comparison.get("minimum_p95_delta_ms"),
        "budget.comparison.minimum_p95_delta_ms",
    )


def validate_result(result: dict[str, Any], label: str) -> None:
    if result.get("schema_version") != 1 or result.get("kind") != INPUT_KIND:
        raise ComparisonError(f"{label} is not a version 1 query-performance result")
    if result.get("status") not in ("passed", "failed"):
        raise ComparisonError(f"{label}.status must be passed or failed")
    if not isinstance(result.get("repository"), dict):
        raise ComparisonError(f"{label}.repository must be an object")
    if not isinstance(result.get("environment"), dict):
        raise ComparisonError(f"{label}.environment must be an object")
    if not isinstance(result.get("seed"), dict):
        raise ComparisonError(f"{label}.seed must be an object")
    if not isinstance(result.get("query"), dict):
        raise ComparisonError(f"{label}.query must be an object")


def validate_same_environment(
    reference: dict[str, Any], result: dict[str, Any], label: str
) -> None:
    for field in ("platform", "machine", "rustc", "cargo", "logical_cpus"):
        reference_value = reference["environment"].get(field)
        result_value = result["environment"].get(field)
        if reference_value != result_value:
            raise ComparisonError(
                f"environment mismatch for {label}.{field}: "
                f"reference={reference_value!r}, result={result_value!r}"
            )


def validate_same_workload(
    result: dict[str, Any], budget: dict[str, Any], label: str
) -> None:
    workload = budget.get("workload")
    if not isinstance(workload, dict):
        raise ComparisonError("budget.workload must be an object")
    fields = {
        "requested_books": workload.get("books"),
        "metadata_seed": workload.get("seed"),
        "cover_every": workload.get("cover_every"),
    }
    for field, expected in fields.items():
        value = result["seed"].get(field)
        if value != expected:
            raise ComparisonError(
                f"seed workload mismatch for {field}: expected={expected!r}, "
                f"{label}={value!r}"
            )
    expected_books = workload.get("books")
    if result["query"].get("library_books") != expected_books:
        raise ComparisonError(f"{label} query library size does not match the budget")


def scenario_p95(result: dict[str, Any], label: str) -> dict[str, float]:
    decisions = result["query"].get("decisions")
    if not isinstance(decisions, list) or not decisions:
        raise ComparisonError(f"{label}.query.decisions must be a non-empty list")
    scenarios: dict[str, float] = {}
    for index, decision in enumerate(decisions):
        if not isinstance(decision, dict):
            raise ComparisonError(f"{label} decision {index} must be an object")
        name = decision.get("name")
        if not isinstance(name, str) or not name or name in scenarios:
            raise ComparisonError(f"{label} decision {index} has an invalid name")
        scenarios[name] = positive_number(
            decision.get("p95_ms"), f"{label} decision {name!r}.p95_ms"
        )
    return scenarios


def result_identity(result: dict[str, Any]) -> dict[str, Any]:
    return {
        "commit": result["repository"].get("commit"),
        "branch": result["repository"].get("branch"),
        "dirty": result["repository"].get("dirty"),
        "status": result.get("status"),
    }


def positive_number(value: Any, context: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ComparisonError(f"{context} must be a finite number greater than zero")
    number = float(value)
    if not math.isfinite(number) or number <= 0:
        raise ComparisonError(f"{context} must be a finite number greater than zero")
    return number


def non_negative_number(value: Any, context: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ComparisonError(f"{context} must be a finite non-negative number")
    number = float(value)
    if not math.isfinite(number) or number < 0:
        raise ComparisonError(f"{context} must be a finite non-negative number")
    return number


def append_github_summary(path: pathlib.Path, report: dict[str, Any]) -> None:
    """Append a concise paired result table, including failures, to GitHub Actions."""

    with path.open("a", encoding="utf-8") as summary:
        summary.write("## Paired p95 performance comparison\n\n")
        summary.write(f"Status: **{report['status']}**\n\n")
        decisions = report.get("decisions")
        if isinstance(decisions, list):
            summary.write("| Scenario | Base p95 | Candidate p95 | Change | Result |\n")
            summary.write("| --- | ---: | ---: | ---: | --- |\n")
            for decision in decisions:
                result = "pass" if decision["passed"] else "fail"
                summary.write(
                    f"| `{decision['name']}` | {decision['base_p95_ms']:.3f} ms | "
                    f"{decision['candidate_p95_ms']:.3f} ms | "
                    f"{decision['regression_percent']:+.1f}% | {result} |\n"
                )
            summary.write("\n")
        if "error" in report:
            summary.write(f"Error: {report['error']}\n\n")


def read_json(path: pathlib.Path) -> dict[str, Any]:
    with path.open(encoding="utf-8") as source:
        value = json.load(source)
    if not isinstance(value, dict):
        raise ComparisonError(f"expected a JSON object in {path}")
    return value


def write_json(path: pathlib.Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    with temporary.open("w", encoding="utf-8") as destination:
        json.dump(value, destination, indent=2)
        destination.write("\n")
    temporary.replace(path)


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
