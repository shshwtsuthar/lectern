"""Unit tests for the deterministic performance-regression runner."""

from __future__ import annotations

import importlib.util
import pathlib
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("performance_regression.py")
SPEC = importlib.util.spec_from_file_location("lectern_performance_regression", MODULE_PATH)
assert SPEC and SPEC.loader
REGRESSION = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REGRESSION)


def budget() -> dict:
    return {
        "schema_version": 1,
        "kind": "lectern-query-regression-budget",
        "workload": {
            "books": 50,
            "seed": 7,
            "cover_every": 0,
            "warmup_iterations": 2,
            "measured_iterations": 3,
            "full_library_scenarios": ["sort_title", "filter_unchecked_assets"],
        },
        "budgets": {
            "search_title_prefix": {"max_p95_ms": 10},
            "sort_title": {"max_p95_ms": 20},
            "filter_unchecked_assets": {
                "max_p95_ms": 25,
                "max_p95_ratio_to": "sort_title",
                "max_p95_ratio": 1.5,
            },
        },
    }


def query_result(*, health_p95: float = 12.0) -> dict:
    def scenario(name: str, result_count: int, p95: float) -> dict:
        return {
            "name": name,
            "result_count": result_count,
            "latency_ms": {"p95": p95},
            "samples_ns": [1_000_000, 2_000_000, 3_000_000],
        }

    return {
        "library_books": 50,
        "warmup_iterations": 2,
        "measured_iterations": 3,
        "scenarios": [
            scenario("search_title_prefix", 4, 4.0),
            scenario("sort_title", 50, 10.0),
            scenario("filter_unchecked_assets", 50, health_p95),
        ],
    }


class PerformanceRegressionTests(unittest.TestCase):
    def test_load_budget_rejects_unpaired_ratio_fields(self) -> None:
        invalid = budget()
        del invalid["budgets"]["filter_unchecked_assets"]["max_p95_ratio"]

        with self.assertRaisesRegex(REGRESSION.RegressionError, "both ratio fields"):
            REGRESSION.validate_budget(invalid)

    def test_evaluate_query_result_accepts_within_budget_workload(self) -> None:
        decisions = REGRESSION.evaluate_query_result(query_result(), budget())

        self.assertTrue(all(decision["passed"] for decision in decisions))
        health = next(
            decision
            for decision in decisions
            if decision["name"] == "filter_unchecked_assets"
        )
        self.assertEqual(health["p95_ratio_to"], "sort_title")
        self.assertEqual(health["p95_ratio"], 1.2)

    def test_evaluate_query_result_reports_relative_regression(self) -> None:
        decisions = REGRESSION.evaluate_query_result(query_result(health_p95=16.0), budget())

        health = next(
            decision
            for decision in decisions
            if decision["name"] == "filter_unchecked_assets"
        )
        self.assertFalse(health["passed"])
        self.assertEqual(health["p95_ratio"], 1.6)

    def test_evaluate_query_result_requires_versioned_scenario_set(self) -> None:
        invalid = query_result()
        invalid["scenarios"].pop()

        with self.assertRaisesRegex(REGRESSION.RegressionError, "do not match"):
            REGRESSION.evaluate_query_result(invalid, budget())


if __name__ == "__main__":
    unittest.main()
