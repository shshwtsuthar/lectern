"""Contract tests for the organisation and curation performance workloads."""

from __future__ import annotations

import json
import pathlib
import unittest
from typing import Any


BENCHMARKS = pathlib.Path(__file__).resolve().parent


def load_budget(name: str) -> dict[str, Any]:
    with (BENCHMARKS / name).open(encoding="utf-8") as handle:
        budget = json.load(handle)
    if not isinstance(budget, dict):
        raise AssertionError(f"{name} must contain a JSON object")
    return budget


class OrganisationPerformanceContractTests(unittest.TestCase):
    def assert_common_library(self, workload: dict[str, Any]) -> None:
        self.assertEqual(workload["books"], 50_000)
        self.assertEqual(workload["contributors"], 20_000)
        self.assertEqual(workload["series"], 2_500)
        self.assertEqual(workload["tags"], 500)
        self.assertEqual(workload["tags_per_book"], 8)
        self.assertEqual(
            workload["contributors_per_book"],
            {"minimum": 1, "maximum": 4},
        )
        self.assertEqual(workload["series_membership_percent"], 70)
        self.assertEqual(workload["saved_searches"], 250)

    def assert_scenarios_have_budgets(self, budget: dict[str, Any]) -> None:
        scenarios = budget["workload"]["scenarios"]
        self.assertEqual(len(scenarios), len(set(scenarios)))
        self.assertEqual(set(scenarios), set(budget["budgets"]))
        self.assertEqual(budget["comparison"]["paired_runs"], 3)
        self.assertEqual(
            budget["comparison"]["max_p95_regression_percent"], 10
        )

    def test_query_contract_covers_every_required_projection(self) -> None:
        budget = load_budget("organisation-query-regression-v1.json")
        self.assertEqual(budget["kind"], "lectern-organisation-regression-budget")
        workload = budget["workload"]
        self.assert_common_library(workload)
        self.assertEqual(workload["page_size"], 128)
        self.assertEqual(workload["autocomplete_limit"], 50)
        self.assertEqual(workload["warmup_iterations"], 10)
        self.assertEqual(workload["measured_iterations"], 40)
        self.assert_scenarios_have_budgets(budget)
        self.assertTrue(
            {
                "exact_ids",
                "matching_counts",
                "stable_order",
                "unique_book_rows",
                "covering_query_plans",
                "bounded_autocomplete",
            }.issubset(workload["correctness"])
        )
        for scenario in workload["scenarios"]:
            self.assertEqual(budget["budgets"][scenario]["max_p95_ms"], 50)

    def test_bulk_contract_bounds_transaction_memory_and_paints(self) -> None:
        budget = load_budget("bulk-tags-regression-v1.json")
        workload = budget["workload"]
        self.assert_common_library(workload)
        self.assertEqual(workload["matching_books"], 10_000)
        self.assertEqual(workload["tags_added"], 2)
        self.assertEqual(workload["tags_removed"], 1)
        self.assertEqual(workload["compositor_samples"], 40)
        self.assert_scenarios_have_budgets(budget)
        mutation = budget["budgets"]["bulk_tag_apply_and_refresh"]
        self.assertEqual(mutation["max_p95_ms"], 500)
        self.assertEqual(mutation["max_peak_rss_delta_bytes"], 32 * 1024 * 1024)
        self.assertEqual(
            budget["budgets"]["selection_dispatch_to_busy_paint"]["max_p95_ms"],
            50,
        )
        self.assertEqual(
            budget["budgets"]["completion_to_refreshed_grid_paint"]["max_p95_ms"],
            50,
        )

    def test_migration_contract_uses_independent_version_five_copies(self) -> None:
        budget = load_budget("organisation-migration-regression-v1.json")
        workload = budget["workload"]
        self.assertEqual(workload["source_schema_version"], 5)
        self.assertEqual(workload["books"], 50_000)
        self.assertGreaterEqual(workload["measured_iterations"], 20)
        self.assert_scenarios_have_budgets(budget)
        migration = budget["budgets"]["migrate_version_five_library"]
        self.assertEqual(migration["max_p95_ms"], 5_000)
        self.assertEqual(migration["max_peak_rss_bytes"], 256 * 1024 * 1024)
        self.assertIn(
            "failed_migration_preserves_version_five_database",
            workload["correctness"],
        )

    def test_compositor_contract_retains_every_sample_and_budget(self) -> None:
        budget = load_budget("organisation-compositor-regression-v1.json")
        workload = budget["workload"]
        self.assertEqual(workload["measured_iterations"], 40)
        self.assert_scenarios_have_budgets(budget)
        for scenario in workload["scenarios"]:
            self.assertEqual(budget["budgets"][scenario]["max_p95_ms"], 50)


if __name__ == "__main__":
    unittest.main()
