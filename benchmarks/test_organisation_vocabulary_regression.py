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
    with (BENCHMARKS / "organisation-vocabulary-regression-v2.json").open(
        encoding="utf-8"
    ) as source:
        return json.load(source)


def valid_result() -> dict:
    workload = checked_in_budget()["workload"]
    scenarios = []
    for name in workload["scenarios"]:
        manager = name == "manager_search_page"
        scenarios.append(
            {
                "name": name,
                "successful_operations": workload["warmup_iterations"]
                + workload["measured_iterations"],
                "books_affected_per_operation": 0
                if manager
                else workload["matching_books"],
                "saved_searches_affected_per_operation": 0
                if manager
                else workload["saved_searches"],
                "refreshed_result_count": 100 if manager else workload["page_size"],
                "samples_ns": [1_000_000] * workload["measured_iterations"],
                "latency_ms": {"p95": 1.0},
            }
        )
    return {
        "kind": "organisation-vocabulary",
        "library_books": workload["books"],
        "matching_books": workload["matching_books"],
        "saved_searches": workload["saved_searches"],
        "warmup_iterations": workload["warmup_iterations"],
        "measured_iterations": workload["measured_iterations"],
        "page_size": workload["page_size"],
        "verified_checks": workload["correctness"],
        "peak_rss_delta_bytes": 1024,
        "scenarios": scenarios,
    }


class OrganisationVocabularyContractTests(unittest.TestCase):
    def test_checked_in_workload_is_bounded_and_complete(self) -> None:
        with (BENCHMARKS / "organisation-vocabulary-regression-v2.json").open(
            encoding="utf-8"
        ) as source:
            budget = json.load(source)

        workload = budget["workload"]
        self.assertEqual(budget["schema_version"], 1)
        self.assertEqual(workload["query_mode"], "organisation-vocabulary")
        self.assertEqual(workload["books"], 50_000)
        self.assertEqual(workload["matching_books"], 10_000)
        self.assertEqual(workload["saved_searches"], 250)
        self.assertEqual(workload["warmup_iterations"], 10)
        self.assertEqual(workload["measured_iterations"], 40)
        self.assertEqual(set(workload["scenarios"]), set(budget["budgets"]))
        self.assertEqual(len(workload["correctness"]), 9)
        self.assertEqual(budget["budgets"]["manager_search_page"]["max_p95_ms"], 50)
        for name, scenario in budget["budgets"].items():
            if name != "manager_search_page":
                self.assertEqual(scenario["max_p95_ms"], 1_000)
            self.assertEqual(scenario["max_peak_rss_delta_bytes"], 32 * 1024 * 1024)

    def test_runner_accepts_exact_results_and_routes_commands(self) -> None:
        budget = REGRESSION.validate_budget(checked_in_budget())
        decisions = REGRESSION.evaluate_organisation_vocabulary_result(
            valid_result(), budget
        )
        self.assertEqual(len(decisions), 4)
        self.assertTrue(all(decision["passed"] for decision in decisions))
        workload = budget["workload"]
        seed = REGRESSION.seed_command(
            "organisation-vocabulary",
            pathlib.Path("library.sqlite3"),
            pathlib.Path("seed.json"),
            workload,
        )
        mutation = REGRESSION.workload_command(
            "organisation-vocabulary",
            pathlib.Path("library.sqlite3"),
            pathlib.Path("vocabulary.json"),
            workload,
        )
        self.assertIn("organisation-query-benchmark", seed)
        self.assertIn("organisation-vocabulary-benchmark", mutation)

    def test_runner_rejects_partial_counts_and_samples(self) -> None:
        result = valid_result()
        result["scenarios"][1]["books_affected_per_operation"] -= 1
        with self.assertRaisesRegex(REGRESSION.RegressionError, "book count"):
            REGRESSION.evaluate_organisation_vocabulary_result(
                result, checked_in_budget()
            )
        result = valid_result()
        result["scenarios"][0]["samples_ns"].pop()
        with self.assertRaisesRegex(REGRESSION.RegressionError, "sample count"):
            REGRESSION.evaluate_organisation_vocabulary_result(
                result, checked_in_budget()
            )


if __name__ == "__main__":
    unittest.main()
