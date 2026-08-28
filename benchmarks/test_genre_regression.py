"""Validation tests for the fixed-genre storage regression workload."""

from __future__ import annotations

import importlib.util
import json
import pathlib
import unittest


BENCHMARKS = pathlib.Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "lectern_performance_regression",
    BENCHMARKS / "performance_regression.py",
)
assert SPEC and SPEC.loader
REGRESSION = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REGRESSION)


def checked_in_budget() -> dict:
    with (BENCHMARKS / "genres-regression-v1.json").open(encoding="utf-8") as source:
        return json.load(source)


def valid_result() -> dict:
    workload = checked_in_budget()["workload"]
    return {
        "kind": "genre-performance",
        "library_books": workload["books"],
        "catalog_genres": workload["catalog_genres"],
        "detail_genres": workload["detail_genres"],
        "warmup_iterations": workload["warmup_iterations"],
        "measured_iterations": workload["measured_iterations"],
        "verified_checks": workload["correctness"],
        "peak_rss_delta_bytes": 8 * 1024 * 1024,
        "scenarios": [
            {
                "name": name,
                "successful_operations": 50,
                "latency_ms": {"p95": 5.0},
                "samples_ns": [5_000_000] * 40,
            }
            for name in workload["scenarios"]
        ],
    }


class GenreRegressionTests(unittest.TestCase):
    def test_checked_in_budget_is_accepted(self) -> None:
        budget = REGRESSION.validate_budget(checked_in_budget())
        self.assertEqual(budget["workload"]["genre_fixture_version"], 1)

    def test_valid_result_passes_latency_and_memory_budgets(self) -> None:
        decisions = REGRESSION.evaluate_genre_result(valid_result(), checked_in_budget())
        self.assertEqual(len(decisions), 2)
        self.assertTrue(all(decision["passed"] for decision in decisions))

    def test_catalog_and_samples_must_reconcile(self) -> None:
        result = valid_result()
        result["catalog_genres"] = 29
        with self.assertRaisesRegex(REGRESSION.RegressionError, "catalog_genres"):
            REGRESSION.evaluate_genre_result(result, checked_in_budget())

        result = valid_result()
        result["scenarios"][0]["samples_ns"].pop()
        with self.assertRaisesRegex(REGRESSION.RegressionError, "sample count"):
            REGRESSION.evaluate_genre_result(result, checked_in_budget())


if __name__ == "__main__":
    unittest.main()
