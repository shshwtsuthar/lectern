import importlib.util
import json
import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
BENCHMARKS = ROOT / "benchmarks"
SPEC = importlib.util.spec_from_file_location(
    "performance_regression", BENCHMARKS / "performance_regression.py"
)
assert SPEC is not None and SPEC.loader is not None
REGRESSION = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REGRESSION)


def checked_in_budget() -> dict:
    with (BENCHMARKS / "organisation-query-regression-v3.json").open(
        encoding="utf-8"
    ) as source:
        return json.load(source)


def valid_result() -> dict:
    budget = checked_in_budget()
    workload = budget["workload"]
    return {
        "kind": "organisation-query",
        "library_books": workload["books"],
        "warmup_iterations": workload["warmup_iterations"],
        "measured_iterations": workload["measured_iterations"],
        "page_size": workload["page_size"],
        "autocomplete_limit": workload["autocomplete_limit"],
        "verified_checks": workload["correctness"],
        "query_plans": [
            {
                "required_index": index,
                "details": [f"SEARCH USING COVERING INDEX {index}"],
            }
            for index in (
                "book_contributors_contributor_role_book_idx",
                "series_memberships_series_index_book_idx",
                "series_memberships_series_number_uidx",
                "book_tags_tag_book_idx",
                "book_identifiers_type_book_idx",
            )
        ],
        "scenarios": [
            {
                "name": name,
                "successful_operations": workload["warmup_iterations"]
                + workload["measured_iterations"],
                "observed_results": 50,
                "samples_ns": [1_000_000] * workload["measured_iterations"],
                "latency_ms": {"p95": 1.0},
            }
            for name in workload["scenarios"]
        ],
    }


class OrganisationQueryRegressionTests(unittest.TestCase):
    def test_checked_in_budget_and_valid_result_are_accepted(self) -> None:
        budget = REGRESSION.validate_budget(checked_in_budget())
        decisions = REGRESSION.evaluate_organisation_query_result(
            valid_result(), budget
        )
        self.assertEqual(len(decisions), 9)
        self.assertTrue(all(decision["passed"] for decision in decisions))

    def test_missing_plan_or_bad_sample_is_rejected(self) -> None:
        result = valid_result()
        result["query_plans"].pop()
        with self.assertRaisesRegex(REGRESSION.RegressionError, "plans"):
            REGRESSION.evaluate_organisation_query_result(
                result, checked_in_budget()
            )

        result = valid_result()
        result["scenarios"][0]["samples_ns"].pop()
        with self.assertRaisesRegex(REGRESSION.RegressionError, "sample count"):
            REGRESSION.evaluate_organisation_query_result(
                result, checked_in_budget()
            )

    def test_commands_route_through_query_benchmark_binary(self) -> None:
        workload = checked_in_budget()["workload"]
        seed = REGRESSION.seed_command(
            "organisation-query",
            pathlib.Path("library.sqlite3"),
            pathlib.Path("seed.json"),
            workload,
        )
        query = REGRESSION.workload_command(
            "organisation-query",
            pathlib.Path("library.sqlite3"),
            pathlib.Path("queries.json"),
            workload,
        )
        self.assertIn("organisation-query-benchmark", seed)
        self.assertIn("organisation-query-benchmark", query)
        self.assertIn("seed", seed)
        self.assertIn("query", query)


if __name__ == "__main__":
    unittest.main()
