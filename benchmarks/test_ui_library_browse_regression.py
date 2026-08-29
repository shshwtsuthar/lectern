"""Validation tests for the metadata-browser GPUI regression."""

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
    with (BENCHMARKS / "ui-library-browse-regression-v1.json").open(
        encoding="utf-8"
    ) as source:
        return json.load(source)


def valid_result() -> dict:
    budget = checked_in_budget()
    workload = budget["workload"]
    return {
        "kind": "lectern-ui-library-browse-performance",
        "library_books": workload["books"],
        "warmup_iterations": workload["warmup_iterations"],
        "measured_iterations": workload["measured_iterations"],
        "raw_samples": [f"sample-{index}.json" for index in range(45)],
        "correctness": REGRESSION.expected_ui_library_browse_correctness(workload),
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


class UiLibraryBrowseRegressionTests(unittest.TestCase):
    def test_checked_in_budget_is_accepted(self) -> None:
        budget = REGRESSION.validate_budget(checked_in_budget())
        self.assertEqual(budget["workload"]["group_count"], 28)
        self.assertEqual(budget["workload"]["page_size"], 128)

    def test_valid_result_passes_latency_and_memory_budgets(self) -> None:
        decisions = REGRESSION.evaluate_ui_library_browse_result(
            valid_result(), checked_in_budget()
        )
        self.assertEqual(len(decisions), 3)
        self.assertTrue(all(decision["passed"] for decision in decisions))

    def test_scope_marker_and_latency_are_enforced(self) -> None:
        result = valid_result()
        result["correctness"]["markers"].remove(
            "compact_scoped_selection_descriptor"
        )
        with self.assertRaisesRegex(REGRESSION.RegressionError, "correctness"):
            REGRESSION.evaluate_ui_library_browse_result(
                result, checked_in_budget()
            )

        result = valid_result()
        result["scenarios"][0]["samples_ns"] = [60_000_000] * 40
        result["scenarios"][0]["latency_ms"]["p95"] = 60.0
        decisions = REGRESSION.evaluate_ui_library_browse_result(
            result, checked_in_budget()
        )
        self.assertTrue(any(not decision["passed"] for decision in decisions))


if __name__ == "__main__":
    unittest.main()
