import importlib.util
import json
import pathlib
import tempfile
import unittest


BENCHMARKS = pathlib.Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "performance_regression", BENCHMARKS / "performance_regression.py"
)
assert SPEC is not None and SPEC.loader is not None
REGRESSION = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REGRESSION)


def checked_in_budget() -> dict:
    with (BENCHMARKS / "bulk-tags-regression-v2.json").open(encoding="utf-8") as source:
        return json.load(source)


def valid_storage_result() -> dict:
    workload = checked_in_budget()["workload"]
    matching = workload["matching_books"]
    return {
        "kind": "organisation-bulk-tags",
        "library_books": workload["books"],
        "matching_books": matching,
        "warmup_iterations": workload["warmup_iterations"],
        "measured_iterations": workload["measured_iterations"],
        "page_size": workload["page_size"],
        "verified_checks": workload["correctness"],
        "selection_materialized_summaries": 0,
        "peak_rss_delta_bytes": 1024,
        "scenarios": [
            {
                "name": "bulk_tag_apply_and_refresh",
                "successful_operations": workload["warmup_iterations"]
                + workload["measured_iterations"],
                "books_matched_per_operation": matching,
                "relationships_added_per_operation": matching * workload["tags_added"],
                "relationships_removed_per_operation": matching
                * workload["tags_removed"],
                "tags_created_per_operation": workload["tags_added"],
                "refreshed_result_count": workload["page_size"],
                "latency_ms": {"p95": 10.0},
                "samples_ns": [10_000_000] * workload["measured_iterations"],
            }
        ],
    }


def valid_desktop_result() -> dict:
    workload = checked_in_budget()["workload"]
    samples = workload["compositor_samples"]
    markers = [
        "selection_busy_state_presented",
        "compact_all_matching_descriptor",
        "bulk_tag_panel_presented",
        "atomic_apply_completed",
        "selection_cleared_after_success",
        "refreshed_grid_presented",
    ]
    return {
        "schema_version": 1,
        "kind": "lectern-gpui-bulk-tags-performance",
        "library_books": workload["books"],
        "matching_books": workload["matching_books"],
        "warmup_iterations": workload["warmup_iterations"],
        "measured_iterations": samples,
        "forward_operations": 25,
        "inverse_operations": 25,
        "raw_samples": ["bulk-tag-gpui-raw.json"],
        "correctness": markers,
        "scenarios": [
            {
                "name": "selection_dispatch_to_busy_paint",
                "latency_ms": {"p95": 1.0},
                "samples_ns": [1_000_000] * samples,
                "peak_rss_bytes": 1024,
            },
            {
                "name": "completion_to_refreshed_grid_paint",
                "latency_ms": {"p95": 1.0},
                "samples_ns": [1_000_000] * samples,
                "peak_rss_bytes": 1024,
            },
        ],
    }


class BulkTagRegressionContractTests(unittest.TestCase):
    def test_runner_accepts_exact_storage_and_compositor_results(self) -> None:
        budget = REGRESSION.validate_budget(checked_in_budget())
        storage = REGRESSION.evaluate_bulk_tag_result(valid_storage_result(), budget)
        desktop = REGRESSION.evaluate_bulk_tag_desktop_result(
            valid_desktop_result(), budget
        )
        decisions = storage + desktop
        self.assertEqual(
            {decision["name"] for decision in decisions}, set(budget["budgets"])
        )
        self.assertTrue(all(decision["passed"] for decision in decisions))

    def test_runner_rejects_partial_samples_and_inexact_counts(self) -> None:
        storage = valid_storage_result()
        storage["scenarios"][0]["relationships_added_per_operation"] -= 1
        with self.assertRaisesRegex(REGRESSION.RegressionError, "did not reconcile"):
            REGRESSION.evaluate_bulk_tag_result(storage, checked_in_budget())

        desktop = valid_desktop_result()
        desktop["scenarios"][1]["samples_ns"].pop()
        with self.assertRaisesRegex(REGRESSION.RegressionError, "sample count"):
            REGRESSION.evaluate_bulk_tag_desktop_result(
                desktop, checked_in_budget()
            )

    def test_commands_route_through_query_seed_bulk_and_desktop_binaries(self) -> None:
        workload = checked_in_budget()["workload"]
        seed = REGRESSION.seed_command(
            "bulk-tags",
            pathlib.Path("library.sqlite3"),
            pathlib.Path("seed.json"),
            workload,
        )
        mutation = REGRESSION.workload_command(
            "bulk-tags",
            pathlib.Path("library.sqlite3"),
            pathlib.Path("bulk.json"),
            workload,
        )
        self.assertIn("organisation-query-benchmark", seed)
        self.assertIn("organisation-bulk-benchmark", mutation)

    def test_tag_identity_resolution_requires_all_three_tags(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            database = pathlib.Path(directory) / "library.sqlite3"
            connection = REGRESSION.sqlite3.connect(database)
            connection.execute("CREATE TABLE tags(id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
            connection.executemany(
                "INSERT INTO tags(id, name) VALUES (?, ?)",
                [(1, "Bulk baseline"), (2, "Bulk added A 000")],
            )
            connection.commit()
            connection.close()
            with self.assertRaisesRegex(REGRESSION.RegressionError, "identities"):
                REGRESSION.resolve_bulk_tag_ids(database)


if __name__ == "__main__":
    unittest.main()
