"""Contract tests for the Kobo-device discovery and transfer workload."""

from __future__ import annotations

import importlib.util
import pathlib
import unittest


BENCHMARKS = pathlib.Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "kobo_device_regression", BENCHMARKS / "kobo_device_regression.py"
)
assert SPEC and SPEC.loader
REGRESSION = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REGRESSION)


def budget() -> dict:
    value = REGRESSION.read_json(BENCHMARKS / "kobo-device-regression-v1.json")
    REGRESSION.validate_budget(value)
    return value


def result() -> dict:
    workload = budget()["workload"]
    return {
        "schema_version": 1,
        "workload": "kobo-device-v1",
        "books": workload["books"],
        "source_bytes_per_book": workload["source_bytes_per_book"],
        "warmup_iterations": workload["warmup_iterations"],
        "measured_iterations": workload["measured_iterations"],
        "system_enumeration": {
            "minimum_mounted_volumes": 1,
            "maximum_mounted_volumes": 1,
            "p95_ms": 2.0,
            "samples_ns": [2_000_000] * 100,
            "correctness": [
                "production_volume_provider_exercised",
                "stable_system_sample_count",
            ],
        },
        "reconciliation": {
            "candidate_volumes": workload["candidate_volumes"],
            "detected_devices": 1,
            "p95_ms": 1.0,
            "samples_ns": [1_000_000] * 100,
            "correctness": [
                "ordinary_volumes_rejected",
                "single_marker_volume_detected",
                "stable_reconciliation_count",
            ],
        },
        "transfer": {
            "transferred_books_per_iteration": workload["books"],
            "transferred_bytes_per_iteration": workload["books"]
            * workload["source_bytes_per_book"],
            "p95_ms": 1000.0,
            "p05_throughput_mib_per_second": 120.0,
            "samples_ns": [1_000_000_000] * workload["measured_iterations"],
            "throughput_mib_per_second": [120.0]
            * workload["measured_iterations"],
            "peak_rss_delta_bytes": 1_000_000,
            "correctness": [
                "all_books_transferred",
                "exact_source_hashes_preserved",
                "no_partial_files_retained",
                "history_reconciled",
            ],
        },
    }


class KoboDeviceRegressionTests(unittest.TestCase):
    def test_checked_in_budget_and_result_pass(self) -> None:
        decisions = REGRESSION.evaluate(result(), budget())
        self.assertTrue(all(decision["passed"] for decision in decisions))
        self.assertEqual(len(decisions), 5)

    def test_wrong_transfer_correctness_fails_closed(self) -> None:
        candidate = result()
        candidate["transfer"]["correctness"] = []
        with self.assertRaisesRegex(
            REGRESSION.RegressionError, "transfer correctness markers"
        ):
            REGRESSION.evaluate(candidate, budget())


if __name__ == "__main__":
    unittest.main()
