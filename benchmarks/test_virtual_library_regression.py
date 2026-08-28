"""Validation tests for the executable virtual-library performance regression."""

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
    with (BENCHMARKS / "virtual-libraries-regression-v1.json").open(
        encoding="utf-8"
    ) as source:
        return json.load(source)


def valid_result() -> dict:
    budget = checked_in_budget()
    workload = budget["workload"]
    return {
        "schema_version": 1,
        "kind": "virtual-library-performance",
        "library_books": workload["books"],
        "virtual_libraries": workload["virtual_libraries"],
        "detail_memberships": workload["detail_memberships"],
        "autocomplete_limit": workload["autocomplete_limit"],
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


class VirtualLibraryRegressionTests(unittest.TestCase):
    def test_checked_in_budget_is_accepted(self) -> None:
        budget = REGRESSION.validate_budget(checked_in_budget())
        self.assertEqual(budget["workload"]["fixture_version"], 2)
        self.assertEqual(budget["workload"]["virtual_library_fixture_version"], 1)

    def test_valid_result_passes_latency_and_memory_budgets(self) -> None:
        decisions = REGRESSION.evaluate_virtual_library_result(
            valid_result(), checked_in_budget()
        )
        self.assertEqual(len(decisions), 4)
        self.assertTrue(all(decision["passed"] for decision in decisions))

    def test_incorrect_membership_or_excess_memory_is_rejected(self) -> None:
        result = valid_result()
        result["detail_memberships"] = 19
        with self.assertRaisesRegex(REGRESSION.RegressionError, "detail_memberships"):
            REGRESSION.evaluate_virtual_library_result(result, checked_in_budget())

        result = valid_result()
        result["peak_rss_delta_bytes"] = 128 * 1024 * 1024
        decisions = REGRESSION.evaluate_virtual_library_result(
            result, checked_in_budget()
        )
        self.assertTrue(all(not decision["passed"] for decision in decisions))


if __name__ == "__main__":
    unittest.main()
