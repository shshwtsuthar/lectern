"""Contract tests for the GPUI material-cover performance workload."""

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
    scenarios = ["covered_library_render", "material_toggle_to_painted_state"]
    return {
        "schema_version": 1,
        "kind": "lectern-ui-regression-budget",
        "workload": {
            "query_mode": "ui-material-covers",
            "books": 50_000,
            "seed": 20260830,
            "cover_every": 1,
            "page_size": 128,
            "warmup_iterations": 1,
            "measured_iterations": 3,
            "scenarios": scenarios,
            "correctness": ["material_cover_stack_presented"],
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
            "peak_rss_bytes": 180_000_000,
        }

    workload = budget()["workload"]
    return {
        "kind": "lectern-ui-material-cover-performance",
        "library_books": 50_000,
        "warmup_iterations": 1,
        "measured_iterations": 3,
        "raw_samples": ["warmup.json", "one.json", "two.json", "three.json"],
        "correctness": REGRESSION.expected_ui_material_covers_correctness(workload),
        "scenarios": [
            scenario("covered_library_render", [190_000_000, 210_000_000, 200_000_000]),
            scenario(
                "material_toggle_to_painted_state",
                [30_000_000, 34_000_000, 32_000_000],
            ),
        ],
    }


class UiMaterialCoversRegressionTests(unittest.TestCase):
    def test_ui_material_cover_budget_is_valid(self) -> None:
        self.assertEqual(REGRESSION.validate_budget(budget())["kind"], budget()["kind"])

    def test_material_cover_budget_requires_every_rendered_book_to_be_covered(self) -> None:
        candidate = budget()
        candidate["workload"]["cover_every"] = 2
        with self.assertRaisesRegex(REGRESSION.RegressionError, "cover every"):
            REGRESSION.validate_budget(candidate)

    def test_retained_samples_drive_p95_and_decisions(self) -> None:
        decisions = REGRESSION.evaluate_ui_material_covers_result(result(), budget())
        self.assertTrue(all(decision["passed"] for decision in decisions))
        self.assertEqual({decision["sample_count"] for decision in decisions}, {3})

    def test_missing_unique_cover_fails_closed(self) -> None:
        candidate = result()
        candidate["correctness"]["unique_covers"] = 127
        with self.assertRaisesRegex(REGRESSION.RegressionError, "correctness markers"):
            REGRESSION.evaluate_ui_material_covers_result(candidate, budget())


if __name__ == "__main__":
    unittest.main()
