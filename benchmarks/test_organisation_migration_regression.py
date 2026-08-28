"""Validation tests for the executable organisation migration regression."""

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
    with (BENCHMARKS / "organisation-migration-regression-v3.json").open(
        encoding="utf-8"
    ) as source:
        return json.load(source)


def valid_result() -> dict:
    return {
        "library_books": 50_000,
        "source_schema_version": 5,
        "final_schema_version": 11,
        "warmup_iterations": 2,
        "measured_iterations": 20,
        "visible_projections_preserved": True,
        "book_asset_cover_identities_preserved": True,
        "fts_equivalent": True,
        "initial_tags_and_saved_searches_empty": True,
        "schema_invariants_valid": True,
        "canonical_metadata_defaults_valid": True,
        "duplicate_series_numbers_repaired": True,
        "failed_migration_rolled_back": True,
        "scenarios": [
            {
                "name": "migrate_version_five_library",
                "successful_migrations": 22,
                "samples_ns": [3_000_000_000] * 20,
                "latency_ms": {"p95": 3_000.0},
                "peak_rss_bytes": 64 * 1024 * 1024,
            },
            {
                "name": "repair_version_seven_series_numbers",
                "successful_migrations": 22,
                "samples_ns": [500_000_000] * 20,
                "latency_ms": {"p95": 500.0},
                "peak_rss_bytes": 64 * 1024 * 1024,
            },
        ],
    }


class OrganisationMigrationRegressionTests(unittest.TestCase):
    def test_checked_in_budget_is_accepted(self) -> None:
        budget = REGRESSION.validate_budget(checked_in_budget())
        self.assertEqual(budget["workload"]["source_schema_version"], 5)

    def test_validated_result_passes_latency_and_memory_budgets(self) -> None:
        decisions = REGRESSION.evaluate_migration_result(
            valid_result(), checked_in_budget()
        )
        self.assertEqual(len(decisions), 2)
        self.assertTrue(all(decision["passed"] for decision in decisions))

    def test_failed_correctness_or_memory_is_rejected(self) -> None:
        result = valid_result()
        result["fts_equivalent"] = False
        with self.assertRaisesRegex(REGRESSION.RegressionError, "fts_equivalent"):
            REGRESSION.evaluate_migration_result(result, checked_in_budget())

        result = valid_result()
        result["scenarios"][0]["peak_rss_bytes"] = 300 * 1024 * 1024
        decisions = REGRESSION.evaluate_migration_result(
            result, checked_in_budget()
        )
        self.assertFalse(decisions[0]["passed"])

    def test_commands_route_through_the_organisation_binary(self) -> None:
        workload = checked_in_budget()["workload"]
        seed = REGRESSION.seed_command(
            "organisation-migration",
            pathlib.Path("library.sqlite3"),
            pathlib.Path("seed.json"),
            workload,
        )
        migration = REGRESSION.workload_command(
            "organisation-migration",
            pathlib.Path("library.sqlite3"),
            pathlib.Path("migrations.json"),
            workload,
        )
        self.assertIn("seed-migration", seed)
        self.assertIn("migration", migration)
        self.assertIn("organisation-benchmark", seed)
        self.assertIn("organisation-benchmark", migration)


if __name__ == "__main__":
    unittest.main()
