"""Validation tests for the native virtual-library UI regression."""

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
    with (BENCHMARKS / "ui-virtual-library-regression-v1.json").open(
        encoding="utf-8"
    ) as source:
        return json.load(source)


def valid_result() -> dict:
    budget = checked_in_budget()
    workload = budget["workload"]
    correctness = REGRESSION.expected_ui_virtual_library_correctness(workload)
    return {
        "kind": "lectern-ui-virtual-library-performance",
        "library_books": workload["books"],
        "warmup_iterations": 5,
        "measured_iterations": 40,
        "raw_samples": [f"sample-{index}.json" for index in range(45)],
        "correctness": correctness,
        "scenarios": [
            {
                "name": name,
                "samples_ns": [10_000_000] * 40,
                "latency_ms": {"p95": 10.0},
                "peak_rss_bytes": 128 * 1024 * 1024,
            }
            for name in workload["scenarios"]
        ],
    }


class UiVirtualLibraryRegressionTests(unittest.TestCase):
    def test_checked_in_budget_is_accepted(self) -> None:
        budget = REGRESSION.validate_budget(checked_in_budget())
        self.assertEqual(budget["workload"]["fixture_version"], 1)

    def test_valid_result_passes_latency_and_memory_budgets(self) -> None:
        decisions = REGRESSION.evaluate_ui_virtual_library_result(
            valid_result(), checked_in_budget()
        )
        self.assertEqual(len(decisions), 3)
        self.assertTrue(all(decision["passed"] for decision in decisions))

    def test_incorrect_markers_and_latency_are_rejected(self) -> None:
        result = valid_result()
        result["correctness"]["membership_count"] = 2
        with self.assertRaisesRegex(REGRESSION.RegressionError, "correctness"):
            REGRESSION.evaluate_ui_virtual_library_result(
                result, checked_in_budget()
            )

        result = valid_result()
        result["scenarios"][0]["samples_ns"] = [600_000_000] * 40
        result["scenarios"][0]["latency_ms"]["p95"] = 600.0
        decisions = REGRESSION.evaluate_ui_virtual_library_result(
            result, checked_in_budget()
        )
        self.assertTrue(any(not decision["passed"] for decision in decisions))


if __name__ == "__main__":
    unittest.main()
