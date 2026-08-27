"""Contract tests for the GPUI selection performance workload."""

from __future__ import annotations

import importlib.util
import pathlib
import unittest


BENCHMARKS = pathlib.Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "performance_regression", BENCHMARKS / "performance_regression.py"
)
assert SPEC and SPEC.loader
REGRESSION = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REGRESSION)


def budget() -> dict:
    scenarios = [
        "initial_library_render",
        "selection_to_painted_state",
        "confirmation_to_painted_state",
    ]
    return {
        "schema_version": 1,
        "kind": "lectern-ui-regression-budget",
        "workload": {
            "query_mode": "ui-selection",
            "books": 50_000,
            "seed": 20260827,
            "cover_every": 0,
            "page_size": 128,
            "warmup_iterations": 1,
            "measured_iterations": 3,
            "scenarios": scenarios,
            "correctness": ["compact_explicit_selection"],
        },
        "comparison": {
            "paired_runs": 3,
            "max_p95_regression_percent": 10,
            "minimum_p95_delta_ms": 1.0,
        },
        "budgets": {
            name: {"max_p95_ms": 500, "max_peak_rss_bytes": 536_870_912}
            for name in scenarios
        },
    }


def result() -> dict:
    def scenario(name: str, samples: list[int]) -> dict:
        return {
            "name": name,
            "samples_ns": samples,
            "latency_ms": {
                "p95": REGRESSION.nearest_rank_p95(samples) / 1_000_000
            },
            "peak_rss_bytes": 120_000_000,
        }

    workload = budget()["workload"]
    return {
        "kind": "lectern-ui-selection-performance",
        "library_books": 50_000,
        "warmup_iterations": 1,
        "measured_iterations": 3,
        "raw_samples": ["warmup.json", "one.json", "two.json", "three.json"],
        "correctness": REGRESSION.expected_ui_selection_correctness(workload),
        "scenarios": [
            scenario("initial_library_render", [100_000_000, 120_000_000, 110_000_000]),
            scenario("selection_to_painted_state", [10_000_000, 12_000_000, 11_000_000]),
            scenario(
                "confirmation_to_painted_state", [14_000_000, 16_000_000, 15_000_000]
            ),
        ],
    }


class UiSelectionRegressionTests(unittest.TestCase):
    def test_ui_selection_budget_is_valid(self) -> None:
        self.assertEqual(REGRESSION.validate_budget(budget())["kind"], budget()["kind"])

    def test_retained_samples_drive_p95_and_decisions(self) -> None:
        decisions = REGRESSION.evaluate_ui_selection_result(result(), budget())
        self.assertTrue(all(decision["passed"] for decision in decisions))
        self.assertEqual({decision["sample_count"] for decision in decisions}, {3})

    def test_wrong_selection_correctness_fails_closed(self) -> None:
        candidate = result()
        candidate["correctness"]["selected_books"] = 2
        with self.assertRaisesRegex(REGRESSION.RegressionError, "correctness markers"):
            REGRESSION.evaluate_ui_selection_result(candidate, budget())


if __name__ == "__main__":
    unittest.main()
