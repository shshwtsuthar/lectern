"""Contract tests for the standalone Kobo-device GPUI performance workload."""

from __future__ import annotations

import importlib.util
import pathlib
import unittest


BENCHMARKS = pathlib.Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "kobo_device_ui_regression", BENCHMARKS / "kobo_device_ui_regression.py"
)
assert SPEC and SPEC.loader
REGRESSION = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REGRESSION)


class KoboDeviceUiRegressionTests(unittest.TestCase):
    def test_checked_in_budget_is_valid(self) -> None:
        budget = REGRESSION.read_json(
            BENCHMARKS / "kobo-device-ui-regression-v1.json"
        )
        workload = REGRESSION.validate_budget(budget)
        self.assertEqual(workload["listed_books"], 128)
        self.assertEqual(workload["managed_books"], 64)

    def test_sample_validation_fails_closed(self) -> None:
        budget = REGRESSION.read_json(
            BENCHMARKS / "kobo-device-ui-regression-v1.json"
        )
        workload = REGRESSION.validate_budget(budget)
        sample = {
            "schema_version": 1,
            "workload": "kobo-device",
            "initial_render_ms": 100.0,
            "device_to_paint_ms": 20.0,
            "correctness": {
                "device_name": "Kobo eReader",
                "total_bytes": 32_000,
                "free_bytes": 20_000,
                "listed_books": 127,
                "managed_books": 64,
                "markers": list(REGRESSION.EXPECTED_MARKERS),
            },
        }
        with self.assertRaisesRegex(ValueError, "listed-book count"):
            REGRESSION.validate_sample(sample, workload)

    def test_nearest_rank_p95_uses_retained_samples(self) -> None:
        self.assertEqual(REGRESSION.nearest_rank_p95(list(range(1, 41))), 38)


if __name__ == "__main__":
    unittest.main()
