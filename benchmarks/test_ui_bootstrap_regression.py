"""Contract tests for the GPUI bootstrap performance workload."""

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
    return {
        "schema_version": 1,
        "kind": "lectern-ui-regression-budget",
        "workload": {
            "query_mode": "ui-bootstrap",
            "books": 0,
            "seed": 0,
            "cover_every": 0,
            "warmup_iterations": 1,
            "measured_iterations": 3,
            "scenarios": ["initial_render", "click_to_painted_busy_state"],
            "correctness": ["initial_state_presented", "busy_state_presented"],
        },
        "comparison": {
            "paired_runs": 3,
            "max_p95_regression_percent": 10,
            "minimum_p95_delta_ms": 1.0,
        },
        "budgets": {
            "initial_render": {
                "max_p95_ms": 500,
                "max_peak_rss_bytes": 536_870_912,
            },
            "click_to_painted_busy_state": {
                "max_p95_ms": 50,
                "max_peak_rss_bytes": 536_870_912,
            },
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
            "peak_rss_bytes": 100_000_000,
        }

    return {
        "kind": "lectern-ui-bootstrap-performance",
        "library_books": 0,
        "warmup_iterations": 1,
        "measured_iterations": 3,
        "raw_samples": ["warmup.json", "one.json", "two.json", "three.json"],
        "correctness": REGRESSION.expected_ui_correctness(),
        "scenarios": [
            scenario("initial_render", [100_000_000, 120_000_000, 110_000_000]),
            scenario("click_to_painted_busy_state", [10_000_000, 12_000_000, 11_000_000]),
        ],
    }


class UiBootstrapRegressionTests(unittest.TestCase):
    def test_ui_budget_is_valid(self) -> None:
        self.assertEqual(REGRESSION.validate_budget(budget())["kind"], budget()["kind"])

    def test_retained_samples_drive_p95_and_decisions(self) -> None:
        decisions = REGRESSION.evaluate_ui_bootstrap_result(result(), budget())
        self.assertTrue(all(decision["passed"] for decision in decisions))
        self.assertEqual({decision["sample_count"] for decision in decisions}, {3})

    def test_mismatched_p95_fails_closed(self) -> None:
        candidate = result()
        candidate["scenarios"][0]["latency_ms"]["p95"] = 1.0
        with self.assertRaisesRegex(REGRESSION.RegressionError, "retained samples"):
            REGRESSION.evaluate_ui_bootstrap_result(candidate, budget())


if __name__ == "__main__":
    unittest.main()
