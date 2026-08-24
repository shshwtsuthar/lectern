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


def page_budget() -> dict:
    return {
        "schema_version": 1,
        "kind": "lectern-query-regression-budget",
        "workload": {
            "query_mode": "page",
            "books": 50,
            "seed": 7,
            "cover_every": 0,
            "page_size": 8,
            "warmup_iterations": 2,
            "measured_iterations": 3,
            "full_count_scenarios": ["first_page_title", "deep_page_title"],
        },
        "budgets": {
            "first_page_title": {"max_p95_ms": 10},
            "deep_page_title": {"max_p95_ms": 20},
            "first_page_search_filter": {"max_p95_ms": 25},
        },
    }


def page_query_result() -> dict:
    def scenario(
        name: str, offset: int, total_count: int, result_count: int, p95: float
    ) -> dict:
        return {
            "name": name,
            "offset": offset,
            "total_count": total_count,
            "result_count": result_count,
            "latency_ms": {"p95": p95},
            "samples_ns": [1_000_000, 2_000_000, 3_000_000],
        }

    return {
        "library_books": 50,
        "page_size": 8,
        "warmup_iterations": 2,
        "measured_iterations": 3,
        "scenarios": [
            scenario("first_page_title", 0, 50, 8, 4.0),
            scenario("deep_page_title", 42, 50, 8, 8.0),
            scenario("first_page_search_filter", 0, 4, 4, 12.0),
        ],
    }


class PerformanceRegressionTests(unittest.TestCase):
    def test_checked_in_budget_is_valid(self) -> None:
        checked_in = REGRESSION.load_budget(REGRESSION.DEFAULT_BUDGET)

        self.assertEqual(checked_in["workload"]["books"], 50_000)
        self.assertIn("filter_unchecked_assets", checked_in["budgets"])

    def test_checked_in_page_budget_is_valid(self) -> None:
        checked_in = REGRESSION.load_budget(
            pathlib.Path(__file__).with_name("query-page-regression-v1.json")
        )

        self.assertEqual(checked_in["workload"]["page_size"], 128)
        self.assertEqual(checked_in["workload"]["query_mode"], "page")

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

    def test_evaluate_query_result_accepts_paged_workload(self) -> None:
        decisions = REGRESSION.evaluate_query_result(page_query_result(), page_budget())

        self.assertTrue(all(decision["passed"] for decision in decisions))

    def test_evaluate_query_result_rejects_oversized_page(self) -> None:
        invalid = page_query_result()
        invalid["scenarios"][0]["result_count"] = 9

        with self.assertRaisesRegex(REGRESSION.RegressionError, "page size"):
            REGRESSION.evaluate_query_result(invalid, page_budget())


if __name__ == "__main__":
    unittest.main()
