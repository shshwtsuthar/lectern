import importlib.util
import json
import pathlib
import unittest


BENCHMARKS = pathlib.Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "performance_regression", BENCHMARKS / "performance_regression.py"
)
assert SPEC is not None and SPEC.loader is not None
REGRESSION = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REGRESSION)


def checked_in_budget() -> dict:
    with (BENCHMARKS / "saved-searches-regression-v2.json").open(
        encoding="utf-8"
    ) as source:
        return json.load(source)


def valid_result() -> dict:
    workload = checked_in_budget()["workload"]
    expected_results = {
        "bounded_saved_search_manager_page": workload["manager_page_size"],
        "saved_search_apply_first_page": workload["query_page_size"],
        "saved_search_management_cycle": 1,
    }
    return {
        "kind": "organisation-saved-searches",
        "library_books": workload["books"],
        "saved_searches": workload["saved_searches"],
        "warmup_iterations": workload["warmup_iterations"],
        "measured_iterations": workload["measured_iterations"],
        "manager_page_size": workload["manager_page_size"],
        "query_page_size": workload["query_page_size"],
        "verified_checks": workload["correctness"],
        "scenarios": [
            {
                "name": name,
                "successful_operations": workload["warmup_iterations"]
                + workload["measured_iterations"],
                "observed_results": observed,
                "latency_ms": {"p95": 1.0},
                "samples_ns": [1_000_000] * workload["measured_iterations"],
            }
            for name, observed in expected_results.items()
        ],
    }


class SavedSearchRegressionContractTests(unittest.TestCase):
    def test_runner_accepts_exact_saved_search_results(self) -> None:
        budget = REGRESSION.validate_budget(checked_in_budget())
        decisions = REGRESSION.evaluate_saved_search_result(valid_result(), budget)
        self.assertEqual(
            {decision["name"] for decision in decisions}, set(budget["budgets"])
        )
        self.assertTrue(all(decision["passed"] for decision in decisions))

    def test_runner_rejects_partial_samples_and_inexact_counts(self) -> None:
        result = valid_result()
        result["scenarios"][0]["samples_ns"].pop()
        with self.assertRaisesRegex(REGRESSION.RegressionError, "sample count"):
            REGRESSION.evaluate_saved_search_result(result, checked_in_budget())

        result = valid_result()
        result["scenarios"][1]["observed_results"] -= 1
        with self.assertRaisesRegex(REGRESSION.RegressionError, "result count"):
            REGRESSION.evaluate_saved_search_result(result, checked_in_budget())

    def test_commands_use_organisation_seed_and_saved_search_binary(self) -> None:
        workload = checked_in_budget()["workload"]
        seed = REGRESSION.seed_command(
            "saved-searches",
            pathlib.Path("library.sqlite3"),
            pathlib.Path("seed.json"),
            workload,
        )
        measurement = REGRESSION.workload_command(
            "saved-searches",
            pathlib.Path("library.sqlite3"),
            pathlib.Path("saved.json"),
            workload,
        )
        self.assertIn("organisation-query-benchmark", seed)
        self.assertIn("organisation-saved-search-benchmark", measurement)


if __name__ == "__main__":
    unittest.main()
