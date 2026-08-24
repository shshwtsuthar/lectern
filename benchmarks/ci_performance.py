#!/usr/bin/env python3
"""Run Lectern's registered absolute or paired CI performance suites."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import pathlib
import re
import shlex
import subprocess
import sys
from typing import Any


REGISTRY_KIND = "lectern-performance-suite-registry"
RESULT_KIND = "lectern-ci-performance-run"
REPOSITORY = pathlib.Path(__file__).resolve().parents[1]
DEFAULT_REGISTRY = REPOSITORY / "benchmarks/suites-v1.json"
SAFE_NAME = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")


class OrchestrationError(RuntimeError):
    """The registered CI performance run could not be completed safely."""


def parse_arguments(arguments: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run registered Lectern performance suites for CI."
    )
    parser.add_argument(
        "--candidate-repository",
        type=pathlib.Path,
        default=REPOSITORY,
    )
    parser.add_argument(
        "--base-repository",
        type=pathlib.Path,
        help="Detached base worktree; omit for absolute-only scheduled runs",
    )
    parser.add_argument(
        "--registry",
        type=pathlib.Path,
        default=DEFAULT_REGISTRY,
    )
    parser.add_argument("--output-dir", type=pathlib.Path, required=True)
    parser.add_argument(
        "--github-summary",
        type=pathlib.Path,
        help="Append suite results to GitHub's step summary",
    )
    return parser.parse_args(arguments)


def main(arguments: list[str]) -> int:
    options = parse_arguments(arguments)
    candidate_repository = options.candidate_repository.resolve()
    base_repository = (
        options.base_repository.resolve() if options.base_repository is not None else None
    )
    registry_path = resolve_path(candidate_repository, options.registry)
    output = options.output_dir.resolve()
    if output.exists():
        raise OrchestrationError(f"output directory already exists: {output}")
    output.mkdir(parents=True)

    report: dict[str, Any] = {
        "schema_version": 1,
        "kind": RESULT_KIND,
        "started_at_utc": utc_now(),
        "status": "running",
        "mode": "paired" if base_repository is not None else "absolute",
        "registry": str(registry_path),
        "suites": [],
    }
    return_code = 0
    try:
        registry = load_registry(registry_path)
        for suite in registry["suites"]:
            suite_report = run_suite(
                suite,
                candidate_repository=candidate_repository,
                base_repository=base_repository,
                output=output,
                github_summary=options.github_summary,
            )
            report["suites"].append(suite_report)
            if suite_report["status"] != "passed":
                return_code = 2
        report["status"] = "passed" if return_code == 0 else "failed"
    except (OSError, ValueError, subprocess.SubprocessError, OrchestrationError) as error:
        report["status"] = "failed"
        report["error"] = str(error)
        print(f"error: {error}", file=sys.stderr)
        return_code = 2
    finally:
        report["completed_at_utc"] = utc_now()
        write_json(output / "ci-performance.json", report)

    if return_code == 0:
        print(f"CI performance run passed: {output / 'ci-performance.json'}")
    return return_code


def load_registry(path: pathlib.Path) -> dict[str, Any]:
    registry = read_json(path)
    if registry.get("schema_version") != 1 or registry.get("kind") != REGISTRY_KIND:
        raise OrchestrationError("suite registry must use the supported version and kind")
    suites = registry.get("suites")
    if not isinstance(suites, list) or not suites:
        raise OrchestrationError("suite registry must contain at least one suite")
    names: set[str] = set()
    budgets: set[str] = set()
    for index, suite in enumerate(suites):
        context = f"suite {index}"
        if not isinstance(suite, dict):
            raise OrchestrationError(f"{context} must be an object")
        name = suite.get("name")
        if not isinstance(name, str) or SAFE_NAME.fullmatch(name) is None:
            raise OrchestrationError(f"{context}.name must be a safe kebab-case name")
        if name in names:
            raise OrchestrationError(f"suite registry repeats name {name!r}")
        names.add(name)
        budget = suite.get("budget")
        if not isinstance(budget, str) or not safe_relative_path(budget):
            raise OrchestrationError(
                f"{context}.budget must be a safe repository-relative JSON path"
            )
        if budget in budgets:
            raise OrchestrationError(f"suite registry repeats budget {budget!r}")
        budgets.add(budget)
    return registry


def safe_relative_path(value: str) -> bool:
    path = pathlib.PurePosixPath(value)
    return (
        bool(value)
        and not path.is_absolute()
        and ".." not in path.parts
        and path.suffix == ".json"
    )


def run_suite(
    suite: dict[str, Any],
    *,
    candidate_repository: pathlib.Path,
    base_repository: pathlib.Path | None,
    output: pathlib.Path,
    github_summary: pathlib.Path | None,
) -> dict[str, Any]:
    name = suite["name"]
    relative_budget = pathlib.PurePosixPath(suite["budget"])
    candidate_budget = candidate_repository.joinpath(*relative_budget.parts)
    budget = read_json(candidate_budget)
    paired_runs = paired_run_count(budget)
    base_budget = (
        base_repository.joinpath(*relative_budget.parts)
        if base_repository is not None
        else None
    )
    comparable = base_budget is not None and base_budget.is_file()
    suite_output = output / name
    suite_output.mkdir()
    suite_report: dict[str, Any] = {
        "name": name,
        "budget": str(relative_budget),
        "mode": "paired" if comparable else "absolute",
        "status": "running",
        "base_results": [],
        "candidate_results": [],
    }

    if comparable:
        assert base_repository is not None and base_budget is not None
        for index in range(1, paired_runs + 1):
            base_result = measure(
                base_repository,
                base_budget,
                suite_output / f"base-{index}",
            )
            suite_report["base_results"].append(str(base_result))
            candidate_result = measure(
                candidate_repository,
                candidate_budget,
                suite_output / f"candidate-{index}",
            )
            suite_report["candidate_results"].append(str(candidate_result))
        comparison_output = suite_output / "comparison.json"
        command = [
            sys.executable,
            str(candidate_repository / "benchmarks/compare_performance.py"),
        ]
        for path in suite_report["base_results"]:
            command += ["--base-result", path]
        for path in suite_report["candidate_results"]:
            command += ["--candidate-result", path]
        command += [
            "--budget",
            str(candidate_budget),
            "--output",
            str(comparison_output),
        ]
        if github_summary is not None:
            command += ["--github-summary", str(github_summary)]
        return_code = run_command(command, candidate_repository)
        suite_report["comparison"] = str(comparison_output)
        suite_report["status"] = "passed" if return_code == 0 else "failed"
    else:
        candidate_result = measure(
            candidate_repository,
            candidate_budget,
            suite_output / "candidate",
        )
        suite_report["candidate_results"].append(str(candidate_result))
        result = read_json(candidate_result)
        suite_report["status"] = (
            "passed" if result.get("status") == "passed" else "failed"
        )
        if github_summary is not None:
            append_absolute_summary(github_summary, name, result, base_repository is not None)
    return suite_report


def paired_run_count(budget: dict[str, Any]) -> int:
    comparison = budget.get("comparison")
    if not isinstance(comparison, dict):
        raise OrchestrationError("performance budget must contain comparison settings")
    value = comparison.get("paired_runs")
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise OrchestrationError("performance budget paired_runs must be a positive integer")
    return value


def measure(
    repository: pathlib.Path, budget: pathlib.Path, output: pathlib.Path
) -> pathlib.Path:
    command = [
        sys.executable,
        str(repository / "benchmarks/performance_regression.py"),
        "--budget",
        str(budget),
        "--output-dir",
        str(output),
    ]
    return_code = run_command(command, repository)
    result = output / "performance-regression.json"
    if not result.is_file():
        raise OrchestrationError(
            f"measurement exited with {return_code} without producing {result}"
        )
    return result


def run_command(command: list[str], repository: pathlib.Path) -> int:
    print(f"+ {shlex.join(command)}", flush=True)
    result = subprocess.run(command, cwd=repository, check=False)
    return result.returncode


def append_absolute_summary(
    path: pathlib.Path, name: str, result: dict[str, Any], missing_base: bool
) -> None:
    with path.open("a", encoding="utf-8") as summary:
        summary.write(f"## Absolute performance suite: `{name}`\n\n")
        if missing_base:
            summary.write(
                "The base revision has no matching versioned workload; relative comparison "
                "starts after this suite is established.\n\n"
            )
        summary.write(f"Status: **{result.get('status', 'invalid')}**\n\n")
        query = result.get("query")
        decisions = query.get("decisions") if isinstance(query, dict) else None
        if isinstance(decisions, list):
            summary.write("| Scenario | p95 | Absolute limit | Result |\n")
            summary.write("| --- | ---: | ---: | --- |\n")
            for decision in decisions:
                status = "pass" if decision.get("passed") else "fail"
                summary.write(
                    f"| `{decision['name']}` | {decision['p95_ms']:.3f} ms | "
                    f"{decision['max_p95_ms']:.3f} ms | {status} |\n"
                )
            summary.write("\n")


def resolve_path(repository: pathlib.Path, path: pathlib.Path) -> pathlib.Path:
    return path.resolve() if path.is_absolute() else (repository / path).resolve()


def read_json(path: pathlib.Path) -> dict[str, Any]:
    with path.open(encoding="utf-8") as source:
        value = json.load(source)
    if not isinstance(value, dict):
        raise OrchestrationError(f"expected a JSON object in {path}")
    return value


def write_json(path: pathlib.Path, value: Any) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    with temporary.open("w", encoding="utf-8") as destination:
        json.dump(value, destination, indent=2)
        destination.write("\n")
    temporary.replace(path)


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv[1:]))
    except (OSError, ValueError, OrchestrationError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(2) from error
