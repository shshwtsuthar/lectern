"""Unit tests for paired p95 performance comparison."""

from __future__ import annotations

import importlib.util
import pathlib
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("compare_performance.py")
SPEC = importlib.util.spec_from_file_location("lectern_compare_performance", MODULE_PATH)
assert SPEC and SPEC.loader
COMPARE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(COMPARE)


def budget() -> dict:
    return {
        "schema_version": 1,
        "kind": "lectern-query-regression-budget",
        "workload": {"books": 50, "seed": 7, "cover_every": 0},
        "comparison": {
            "paired_runs": 1,
            "max_p95_regression_percent": 10,
            "minimum_p95_delta_ms": 0.5,
        },
        "budgets": {
            "first_page": {"max_p95_ms": 10},
            "search": {"max_p95_ms": 20},
        },
    }


def result(
    first_page: float,
    search: float,
    *,
    commit: str,
    status: str = "passed",
    platform: str = "test-host",
) -> dict:
    return {
        "schema_version": 1,
        "kind": "lectern-query-performance-regression",
        "status": status,
        "repository": {"commit": commit, "branch": "test", "dirty": False},
        "environment": {
            "platform": platform,
            "machine": "x86_64",
            "rustc": "rustc test",
            "cargo": "cargo test",
            "logical_cpus": 8,
        },
        "seed": {
            "requested_books": 50,
            "metadata_seed": 7,
            "cover_every": 0,
        },
        "query": {
            "library_books": 50,
            "decisions": [
                {"name": "first_page", "p95_ms": first_page, "passed": True},
                {"name": "search", "p95_ms": search, "passed": True},
            ],
        },
    }


class ComparePerformanceTests(unittest.TestCase):
    def test_accepts_improvement_and_small_materially_insignificant_change(self) -> None:
        report = COMPARE.compare_results(
            result(2.0, 10.0, commit="base"),
            result(2.4, 9.0, commit="candidate"),
            budget(),
        )

        self.assertTrue(all(decision["passed"] for decision in report["decisions"]))

    def test_rejects_change_exceeding_percentage_and_minimum_delta(self) -> None:
        report = COMPARE.compare_results(
            result(2.0, 10.0, commit="base"),
            result(3.0, 12.0, commit="candidate"),
            budget(),
        )

        self.assertFalse(all(decision["passed"] for decision in report["decisions"]))
        search = next(item for item in report["decisions"] if item["name"] == "search")
        self.assertEqual(search["delta_ms"], 2.0)
        self.assertEqual(search["regression_percent"], 20.0)

    def test_candidate_must_pass_absolute_budget(self) -> None:
        with self.assertRaisesRegex(COMPARE.ComparisonError, "absolute"):
            COMPARE.compare_results(
                result(2.0, 10.0, commit="base"),
                result(3.0, 12.0, commit="candidate", status="failed"),
                budget(),
            )

    def test_rejects_environment_mismatch(self) -> None:
        with self.assertRaisesRegex(COMPARE.ComparisonError, "environment mismatch"):
            COMPARE.compare_results(
                result(2.0, 10.0, commit="base"),
                result(2.0, 10.0, commit="candidate", platform="other-host"),
                budget(),
            )

    def test_uses_median_of_required_run_level_p95_values(self) -> None:
        configured = budget()
        configured["comparison"]["paired_runs"] = 3

        report = COMPARE.compare_result_sets(
            [
                result(2.0, 10.0, commit="base"),
                result(20.0, 11.0, commit="base"),
                result(2.1, 9.0, commit="base"),
            ],
            [
                result(2.1, 10.5, commit="candidate"),
                result(2.2, 40.0, commit="candidate"),
                result(2.0, 10.0, commit="candidate"),
            ],
            configured,
        )

        self.assertTrue(all(decision["passed"] for decision in report["decisions"]))
        first_page = next(
            item for item in report["decisions"] if item["name"] == "first_page"
        )
        self.assertEqual(first_page["base_p95_ms"], 2.1)
        self.assertEqual(first_page["candidate_p95_ms"], 2.1)

    def test_rejects_scenario_mismatch(self) -> None:
        candidate = result(2.0, 10.0, commit="candidate")
        candidate["query"]["decisions"].pop()

        with self.assertRaisesRegex(COMPARE.ComparisonError, "scenarios do not match"):
            COMPARE.compare_results(
                result(2.0, 10.0, commit="base"), candidate, budget()
            )


if __name__ == "__main__":
    unittest.main()
