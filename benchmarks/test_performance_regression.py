"""Unit tests for the deterministic performance-regression runner."""

from __future__ import annotations

import importlib.util
import pathlib
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("performance_regression.py")
SPEC = importlib.util.spec_from_file_location("lectern_performance_regression", MODULE_PATH)
assert SPEC and SPEC.loader
REGRESSION = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REGRESSION)


def budget() -> dict:
    return {
        "schema_version": 1,
        "kind": "lectern-query-regression-budget",
        "workload": {
            "books": 50,
            "seed": 7,
            "cover_every": 0,
            "warmup_iterations": 2,
            "measured_iterations": 3,
            "full_library_scenarios": ["sort_title", "filter_unchecked_assets"],
        },
        "comparison": {
            "paired_runs": 3,
            "max_p95_regression_percent": 10,
            "minimum_p95_delta_ms": 0.5,
        },
        "budgets": {
            "search_title_prefix": {"max_p95_ms": 10},
            "sort_title": {"max_p95_ms": 20},
            "filter_unchecked_assets": {
                "max_p95_ms": 25,
                "max_p95_ratio_to": "sort_title",
                "max_p95_ratio": 1.5,
            },
        },
    }


def query_result(*, health_p95: float = 12.0) -> dict:
    def scenario(name: str, result_count: int, p95: float) -> dict:
        return {
            "name": name,
            "result_count": result_count,
            "latency_ms": {"p95": p95},
            "samples_ns": [1_000_000, 2_000_000, 3_000_000],
        }

    return {
        "library_books": 50,
        "warmup_iterations": 2,
        "measured_iterations": 3,
        "scenarios": [
            scenario("search_title_prefix", 4, 4.0),
            scenario("sort_title", 50, 10.0),
            scenario("filter_unchecked_assets", 50, health_p95),
        ],
    }


def page_budget() -> dict:
    return {
        "schema_version": 1,
        "kind": "lectern-query-regression-budget",
        "workload": {
            "query_mode": "page",
            "books": 50,
            "seed": 7,
            "cover_every": 0,
            "page_size": 8,
            "warmup_iterations": 2,
            "measured_iterations": 3,
            "full_count_scenarios": ["first_page_title", "deep_page_title"],
        },
        "comparison": {
            "paired_runs": 3,
            "max_p95_regression_percent": 10,
            "minimum_p95_delta_ms": 0.25,
        },
        "budgets": {
            "first_page_title": {"max_p95_ms": 10},
            "deep_page_title": {"max_p95_ms": 20},
            "first_page_search_filter": {"max_p95_ms": 25},
        },
    }


def page_query_result() -> dict:
    def scenario(
        name: str, offset: int, total_count: int, result_count: int, p95: float
    ) -> dict:
        return {
            "name": name,
            "offset": offset,
            "total_count": total_count,
            "result_count": result_count,
            "latency_ms": {"p95": p95},
            "samples_ns": [1_000_000, 2_000_000, 3_000_000],
        }

    return {
        "library_books": 50,
        "page_size": 8,
        "warmup_iterations": 2,
        "measured_iterations": 3,
        "scenarios": [
            scenario("first_page_title", 0, 50, 8, 4.0),
            scenario("deep_page_title", 42, 50, 8, 8.0),
            scenario("first_page_search_filter", 0, 4, 4, 12.0),
        ],
    }


def remove_budget() -> dict:
    return {
        "schema_version": 1,
        "kind": "lectern-query-regression-budget",
        "workload": {
            "query_mode": "remove",
            "books": 50,
            "seed": 7,
            "cover_every": 0,
            "page_size": 8,
            "warmup_iterations": 2,
            "measured_iterations": 3,
            "scenarios": ["remove_book_and_refresh"],
        },
        "comparison": {
            "paired_runs": 3,
            "max_p95_regression_percent": 10,
            "minimum_p95_delta_ms": 1.0,
        },
        "budgets": {"remove_book_and_refresh": {"max_p95_ms": 100}},
    }


def remove_result() -> dict:
    return {
        "library_books": 50,
        "final_library_books": 50,
        "page_size": 8,
        "warmup_iterations": 2,
        "measured_iterations": 3,
        "source_files": ["candidate.epub", "candidate.pdf"],
        "source_bytes_unchanged": True,
        "scenarios": [
            {
                "name": "remove_book_and_refresh",
                "successful_removals": 5,
                "refreshed_total": 50,
                "refreshed_result_count": 8,
                "latency_ms": {"p95": 12.0},
                "samples_ns": [1_000_000, 2_000_000, 3_000_000],
            }
        ],
    }


def attach_budget() -> dict:
    return {
        "schema_version": 1,
        "kind": "lectern-query-regression-budget",
        "workload": {
            "query_mode": "attach",
            "books": 50,
            "seed": 7,
            "cover_every": 3,
            "page_size": 8,
            "source_payload_bytes": 8_388_608,
            "warmup_iterations": 2,
            "measured_iterations": 3,
            "scenarios": ["attach_validated_format_and_refresh"],
        },
        "comparison": {
            "paired_runs": 3,
            "max_p95_regression_percent": 10,
            "minimum_p95_delta_ms": 1.0,
        },
        "budgets": {"attach_validated_format_and_refresh": {"max_p95_ms": 150}},
    }


def detach_budget() -> dict:
    return {
        "schema_version": 1,
        "kind": "lectern-query-regression-budget",
        "workload": {
            "query_mode": "detach",
            "books": 50,
            "seed": 7,
            "cover_every": 3,
            "page_size": 8,
            "warmup_iterations": 2,
            "measured_iterations": 3,
            "scenarios": ["detach_asset_and_refresh"],
        },
        "comparison": {
            "paired_runs": 3,
            "max_p95_regression_percent": 10,
            "minimum_p95_delta_ms": 1.0,
        },
        "budgets": {"detach_asset_and_refresh": {"max_p95_ms": 100}},
    }


def detach_result() -> dict:
    return {
        "library_books": 50,
        "final_library_books": 50,
        "page_size": 8,
        "warmup_iterations": 2,
        "measured_iterations": 3,
        "source_files": ["candidate.epub", "candidate.pdf"],
        "source_bytes_unchanged": True,
        "metadata_preserved": True,
        "covers_preserved": True,
        "scenarios": [
            {
                "name": "detach_asset_and_refresh",
                "successful_detaches": 5,
                "refreshed_total": 51,
                "refreshed_result_count": 8,
                "format_total": 10,
                "format_result_count": 8,
                "latency_ms": {"p95": 12.0},
                "samples_ns": [1_000_000, 2_000_000, 3_000_000],
            }
        ],
    }


def attach_result() -> dict:
    return {
        "library_books": 50,
        "final_library_books": 50,
        "initial_pdf_books": 10,
        "final_pdf_books": 15,
        "page_size": 8,
        "source_payload_bytes": 8_388_608,
        "minimum_source_bytes": 8_389_000,
        "maximum_source_bytes": 8_389_000,
        "warmup_iterations": 2,
        "measured_iterations": 3,
        "source_files": [f"attachment-{index}.pdf" for index in range(5)],
        "source_bytes_unchanged": True,
        "metadata_preserved": True,
        "covers_preserved": True,
        "scenarios": [
            {
                "name": "attach_validated_format_and_refresh",
                "validated_publications": 5,
                "successful_attachments": 5,
                "refreshed_total": 15,
                "refreshed_result_count": 8,
                "latency_ms": {"p95": 12.0},
                "samples_ns": [1_000_000, 2_000_000, 3_000_000],
            }
        ],
    }


def reimport_budget() -> dict:
    return {
        "schema_version": 1,
        "kind": "lectern-query-regression-budget",
        "workload": {
            "query_mode": "reimport",
            "books": 50,
            "seed": 7,
            "cover_every": 3,
            "warmup_iterations": 2,
            "measured_iterations": 3,
            "scenarios": ["reimport_known_path"],
        },
        "comparison": {
            "paired_runs": 3,
            "max_p95_regression_percent": 10,
            "minimum_p95_delta_ms": 0.5,
        },
        "budgets": {"reimport_known_path": {"max_p95_ms": 25}},
    }


def replace_budget() -> dict:
    return {
        "schema_version": 1,
        "kind": "lectern-query-regression-budget",
        "workload": {
            "query_mode": "replace",
            "books": 50,
            "seed": 7,
            "cover_every": 3,
            "page_size": 8,
            "source_payload_bytes": 8_388_608,
            "warmup_iterations": 2,
            "measured_iterations": 3,
            "scenarios": ["replace_validated_asset_and_refresh"],
        },
        "comparison": {
            "paired_runs": 3,
            "max_p95_regression_percent": 10,
            "minimum_p95_delta_ms": 1.0,
        },
        "budgets": {"replace_validated_asset_and_refresh": {"max_p95_ms": 150}},
    }


def replace_result() -> dict:
    return {
        "library_books": 50,
        "final_library_books": 50,
        "page_size": 8,
        "source_payload_bytes": 8_388_608,
        "warmup_iterations": 2,
        "measured_iterations": 3,
        "source_files": ["original.pdf", "replacement.pdf"],
        "verified_checks": ["source_bytes", "metadata", "covers", "asset_identity"],
        "scenarios": [
            {
                "name": "replace_validated_asset_and_refresh",
                "validated_publications": 5,
                "successful_replacements": 5,
                "refreshed_total": 51,
                "refreshed_result_count": 8,
                "latency_ms": {"p95": 12.0},
                "samples_ns": [1_000_000, 2_000_000, 3_000_000],
            }
        ],
    }


def export_budget() -> dict:
    return {
        "schema_version": 1,
        "kind": "lectern-query-regression-budget",
        "workload": {
            "query_mode": "export",
            "books": 50,
            "seed": 7,
            "cover_every": 0,
            "source_bytes": 268_435_456,
            "copy_buffer_bytes": 262_144,
            "warmup_iterations": 1,
            "measured_iterations": 3,
            "scenarios": ["export_large_file"],
        },
        "comparison": {
            "paired_runs": 3,
            "max_p95_regression_percent": 10,
            "minimum_p95_delta_ms": 1.0,
        },
        "budgets": {
            "export_large_file": {
                "max_p95_ms": 50,
                "min_p05_throughput_mib_per_second": 100,
                "max_peak_rss_delta_bytes": 16_777_216,
            }
        },
    }


def export_result() -> dict:
    return {
        "library_books": 50,
        "source_bytes": 268_435_456,
        "copy_buffer_bytes": 262_144,
        "warmup_iterations": 1,
        "measured_iterations": 3,
        "peak_rss_delta_bytes": 2_000_000,
        "verified_checks": [
            "exact_bytes",
            "collision_preserved",
            "missing_source_rejected",
            "temporary_cleanup",
        ],
        "scenarios": [
            {
                "name": "export_large_file",
                "successful_exports": 4,
                "latency_ms": {"p95": 2.0},
                "samples_ns": [1_000_000, 2_000_000, 1_500_000],
                "copy_samples_ns": [500_000_000, 510_000_000, 490_000_000],
                "throughput_mib_per_second": {"p05": 501.0},
            }
        ],
    }


def reimport_result() -> dict:
    return {
        "library_books": 50,
        "final_library_books": 50,
        "warmup_iterations": 2,
        "measured_iterations": 3,
        "metadata_preserved": True,
        "assets_preserved": True,
        "covers_preserved": True,
        "scenarios": [
            {
                "name": "reimport_known_path",
                "successful_reimports": 5,
                "latency_ms": {"p95": 2.0},
                "samples_ns": [1_000_000, 2_000_000, 3_000_000],
            }
        ],
    }


class PerformanceRegressionTests(unittest.TestCase):
    def test_checked_in_budget_is_valid(self) -> None:
        checked_in = REGRESSION.load_budget(REGRESSION.DEFAULT_BUDGET)

        self.assertEqual(checked_in["workload"]["books"], 50_000)
        self.assertEqual(checked_in["comparison"]["paired_runs"], 3)
        self.assertIn("filter_unchecked_assets", checked_in["budgets"])

    def test_checked_in_page_budget_is_valid(self) -> None:
        checked_in = REGRESSION.load_budget(
            pathlib.Path(__file__).with_name("query-page-regression-v1.json")
        )

        self.assertEqual(checked_in["workload"]["page_size"], 128)
        self.assertEqual(checked_in["workload"]["query_mode"], "page")

    def test_checked_in_covered_query_budget_is_valid(self) -> None:
        checked_in = REGRESSION.load_budget(
            pathlib.Path(__file__).with_name("query-covered-regression-v1.json")
        )

        self.assertEqual(checked_in["workload"]["books"], 50_000)
        self.assertEqual(checked_in["workload"]["cover_every"], 3)
        self.assertIn("sort_author", checked_in["budgets"])

    def test_checked_in_covered_page_budget_is_valid(self) -> None:
        checked_in = REGRESSION.load_budget(
            pathlib.Path(__file__).with_name("query-page-covered-regression-v1.json")
        )

        self.assertEqual(checked_in["workload"]["query_mode"], "page-covered")
        self.assertEqual(checked_in["workload"]["cover_every"], 3)
        self.assertIn("first_page_author", checked_in["budgets"])

    def test_checked_in_remove_budget_is_valid(self) -> None:
        checked_in = REGRESSION.load_budget(
            pathlib.Path(__file__).with_name("remove-book-regression-v1.json")
        )

        self.assertEqual(checked_in["workload"]["query_mode"], "remove")
        self.assertEqual(checked_in["budgets"]["remove_book_and_refresh"]["max_p95_ms"], 100)

    def test_checked_in_attach_budget_is_valid(self) -> None:
        checked_in = REGRESSION.load_budget(
            pathlib.Path(__file__).with_name("attach-format-regression-v1.json")
        )

        self.assertEqual(checked_in["workload"]["query_mode"], "attach")
        self.assertEqual(checked_in["workload"]["source_payload_bytes"], 8_388_608)

    def test_checked_in_detach_budget_is_valid(self) -> None:
        checked_in = REGRESSION.load_budget(
            pathlib.Path(__file__).with_name("detach-asset-regression-v1.json")
        )

        self.assertEqual(checked_in["workload"]["query_mode"], "detach")
        self.assertEqual(checked_in["workload"]["books"], 50_000)

    def test_checked_in_reimport_budget_is_valid(self) -> None:
        checked_in = REGRESSION.load_budget(
            pathlib.Path(__file__).with_name("reimport-known-path-regression-v1.json")
        )

        self.assertEqual(checked_in["workload"]["query_mode"], "reimport")
        self.assertEqual(checked_in["budgets"]["reimport_known_path"]["max_p95_ms"], 25)

    def test_checked_in_replace_budget_is_valid(self) -> None:
        checked_in = REGRESSION.load_budget(
            pathlib.Path(__file__).with_name("replace-asset-regression-v1.json")
        )

        self.assertEqual(checked_in["workload"]["query_mode"], "replace")
        self.assertEqual(checked_in["workload"]["source_payload_bytes"], 8_388_608)

    def test_checked_in_export_budget_is_valid(self) -> None:
        checked_in = REGRESSION.load_budget(
            pathlib.Path(__file__).with_name("export-asset-regression-v1.json")
        )

        self.assertEqual(checked_in["workload"]["query_mode"], "export")
        self.assertEqual(checked_in["workload"]["source_bytes"], 268_435_456)

    def test_load_budget_rejects_unpaired_ratio_fields(self) -> None:
        invalid = budget()
        del invalid["budgets"]["filter_unchecked_assets"]["max_p95_ratio"]

        with self.assertRaisesRegex(REGRESSION.RegressionError, "both ratio fields"):
            REGRESSION.validate_budget(invalid)

    def test_load_budget_rejects_invalid_comparison_thresholds(self) -> None:
        invalid = budget()
        invalid["comparison"]["minimum_p95_delta_ms"] = -0.1

        with self.assertRaisesRegex(REGRESSION.RegressionError, "non-negative"):
            REGRESSION.validate_budget(invalid)

    def test_load_budget_rejects_zero_paired_runs(self) -> None:
        invalid = budget()
        invalid["comparison"]["paired_runs"] = 0

        with self.assertRaisesRegex(REGRESSION.RegressionError, "greater than zero"):
            REGRESSION.validate_budget(invalid)

    def test_evaluate_query_result_accepts_within_budget_workload(self) -> None:
        decisions = REGRESSION.evaluate_query_result(query_result(), budget())

        self.assertTrue(all(decision["passed"] for decision in decisions))
        health = next(
            decision
            for decision in decisions
            if decision["name"] == "filter_unchecked_assets"
        )
        self.assertEqual(health["p95_ratio_to"], "sort_title")
        self.assertEqual(health["p95_ratio"], 1.2)

    def test_evaluate_query_result_reports_relative_regression(self) -> None:
        decisions = REGRESSION.evaluate_query_result(query_result(health_p95=16.0), budget())

        health = next(
            decision
            for decision in decisions
            if decision["name"] == "filter_unchecked_assets"
        )
        self.assertFalse(health["passed"])
        self.assertEqual(health["p95_ratio"], 1.6)

    def test_evaluate_query_result_requires_versioned_scenario_set(self) -> None:
        invalid = query_result()
        invalid["scenarios"].pop()

        with self.assertRaisesRegex(REGRESSION.RegressionError, "do not match"):
            REGRESSION.evaluate_query_result(invalid, budget())

    def test_evaluate_query_result_accepts_paged_workload(self) -> None:
        decisions = REGRESSION.evaluate_query_result(page_query_result(), page_budget())

        self.assertTrue(all(decision["passed"] for decision in decisions))

    def test_evaluate_query_result_rejects_oversized_page(self) -> None:
        invalid = page_query_result()
        invalid["scenarios"][0]["result_count"] = 9

        with self.assertRaisesRegex(REGRESSION.RegressionError, "page size"):
            REGRESSION.evaluate_query_result(invalid, page_budget())

    def test_evaluate_remove_result_accepts_reconciled_workload(self) -> None:
        decisions = REGRESSION.evaluate_remove_result(remove_result(), remove_budget())

        self.assertTrue(all(decision["passed"] for decision in decisions))

    def test_evaluate_remove_result_requires_untouched_sources(self) -> None:
        invalid = remove_result()
        invalid["source_bytes_unchanged"] = False

        with self.assertRaisesRegex(REGRESSION.RegressionError, "source bytes"):
            REGRESSION.evaluate_remove_result(invalid, remove_budget())

    def test_evaluate_attach_result_accepts_reconciled_workload(self) -> None:
        decisions = REGRESSION.evaluate_attach_result(attach_result(), attach_budget())

        self.assertTrue(all(decision["passed"] for decision in decisions))

    def test_evaluate_detach_result_accepts_reconciled_workload(self) -> None:
        decisions = REGRESSION.evaluate_detach_result(detach_result(), detach_budget())

        self.assertTrue(all(decision["passed"] for decision in decisions))

    def test_evaluate_detach_result_requires_untouched_sources(self) -> None:
        invalid = detach_result()
        invalid["source_bytes_unchanged"] = False

        with self.assertRaisesRegex(REGRESSION.RegressionError, "source bytes"):
            REGRESSION.evaluate_detach_result(invalid, detach_budget())

    def test_evaluate_attach_result_requires_preserved_book_data(self) -> None:
        invalid = attach_result()
        invalid["covers_preserved"] = False

        with self.assertRaisesRegex(REGRESSION.RegressionError, "cached covers"):
            REGRESSION.evaluate_attach_result(invalid, attach_budget())

    def test_evaluate_attach_result_reconciles_format_count(self) -> None:
        invalid = attach_result()
        invalid["final_pdf_books"] = 14

        with self.assertRaisesRegex(REGRESSION.RegressionError, "PDF count"):
            REGRESSION.evaluate_attach_result(invalid, attach_budget())

    def test_evaluate_reimport_result_accepts_reconciled_workload(self) -> None:
        decisions = REGRESSION.evaluate_reimport_result(
            reimport_result(), reimport_budget()
        )

        self.assertTrue(all(decision["passed"] for decision in decisions))

    def test_evaluate_replace_result_accepts_reconciled_workload(self) -> None:
        decisions = REGRESSION.evaluate_replace_result(replace_result(), replace_budget())

        self.assertTrue(all(decision["passed"] for decision in decisions))

    def test_evaluate_replace_result_requires_stable_asset_identity(self) -> None:
        invalid = replace_result()
        invalid["verified_checks"].remove("asset_identity")

        with self.assertRaisesRegex(REGRESSION.RegressionError, "asset identity"):
            REGRESSION.evaluate_replace_result(invalid, replace_budget())

    def test_evaluate_export_result_accepts_bounded_exact_copy(self) -> None:
        decisions = REGRESSION.evaluate_export_result(export_result(), export_budget())

        self.assertTrue(all(decision["passed"] for decision in decisions))

    def test_evaluate_export_result_gates_throughput_and_memory(self) -> None:
        invalid = export_result()
        invalid["peak_rss_delta_bytes"] = 20_000_000
        invalid["scenarios"][0]["throughput_mib_per_second"]["p05"] = 90.0

        decisions = REGRESSION.evaluate_export_result(invalid, export_budget())

        self.assertFalse(decisions[0]["passed"])

    def test_evaluate_reimport_result_requires_preserved_metadata(self) -> None:
        invalid = reimport_result()
        invalid["metadata_preserved"] = False

        with self.assertRaisesRegex(REGRESSION.RegressionError, "metadata"):
            REGRESSION.evaluate_reimport_result(invalid, reimport_budget())


if __name__ == "__main__":
    unittest.main()
