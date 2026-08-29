"""Validation tests for the executable library-browsing performance gate."""

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
    with (BENCHMARKS / "library-browse-regression-v1.json").open(
        encoding="utf-8"
    ) as source:
        return json.load(source)


def valid_result() -> dict:
    budget = checked_in_budget()
    workload = budget["workload"]
    samples = [5_000_000] * workload["measured_iterations"]
    return {
        "schema_version": 1,
        "kind": "library-browse-performance",
        "library_books": workload["books"],
        "contributors": workload["contributors"],
        "series": workload["series"],
        "catalog_genres": workload["catalog_genres"],
        "virtual_libraries": workload["virtual_libraries"],
        "virtual_memberships_per_book": workload[
            "virtual_memberships_per_book"
        ],
        "group_page_size": workload["group_page_size"],
        "book_page_size": workload["page_size"],
        "warmup_iterations": workload["warmup_iterations"],
        "measured_iterations": workload["measured_iterations"],
        "verified_checks": workload["correctness"],
        "peak_rss_delta_bytes": 8 * 1024 * 1024,
        "query_plans": [
            {
                "required_index": index,
                "details": [f"SEARCH USING COVERING INDEX {index}"],
            }
            for index in (
                "book_contributors_contributor_role_book_idx",
                "series_memberships_series_index_book_idx",
                "book_genres_genre_book_idx",
                "book_virtual_libraries_library_book_idx",
            )
        ],
        "scenarios": [
            {
                "name": name,
                "successful_operations": workload["warmup_iterations"]
                + workload["measured_iterations"],
                "observed_results": 28 if name == "genre_groups_first_page" else 100,
                "latency_ms": {"p95": 5.0},
                "samples_ns": samples,
            }
            for name in workload["scenarios"]
        ],
    }


class LibraryBrowseRegressionTests(unittest.TestCase):
    def test_checked_in_budget_and_valid_result_are_accepted(self) -> None:
        budget = REGRESSION.validate_budget(checked_in_budget())
        decisions = REGRESSION.evaluate_library_browse_result(
            valid_result(), budget
        )
        self.assertEqual(len(decisions), 9)
        self.assertTrue(all(decision["passed"] for decision in decisions))

    def test_scope_plan_samples_and_memory_are_enforced(self) -> None:
        result = valid_result()
        result["query_plans"].pop()
        with self.assertRaisesRegex(REGRESSION.RegressionError, "plans"):
            REGRESSION.evaluate_library_browse_result(
                result, checked_in_budget()
            )

        result = valid_result()
        result["scenarios"][0]["samples_ns"].pop()
        with self.assertRaisesRegex(REGRESSION.RegressionError, "sample"):
            REGRESSION.evaluate_library_browse_result(
                result, checked_in_budget()
            )

        result = valid_result()
        result["peak_rss_delta_bytes"] = 128 * 1024 * 1024
        decisions = REGRESSION.evaluate_library_browse_result(
            result, checked_in_budget()
        )
        self.assertTrue(all(not decision["passed"] for decision in decisions))

    def test_commands_reuse_the_v3_seed_and_run_the_browse_binary(self) -> None:
        workload = checked_in_budget()["workload"]
        seed = REGRESSION.seed_command(
            "library-browse",
            pathlib.Path("library.sqlite3"),
            pathlib.Path("seed.json"),
            workload,
        )
        query = REGRESSION.workload_command(
            "library-browse",
            pathlib.Path("library.sqlite3"),
            pathlib.Path("browse.json"),
            workload,
        )
        self.assertIn("organisation-query-benchmark", seed)
        self.assertIn("--fixture-version", seed)
        self.assertIn("library-browse-benchmark", query)


if __name__ == "__main__":
    unittest.main()
