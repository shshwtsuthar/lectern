import json
import pathlib
import unittest


BENCHMARKS = pathlib.Path(__file__).resolve().parent


class OrganisationVocabularyContractTests(unittest.TestCase):
    def test_checked_in_workload_is_bounded_and_complete(self) -> None:
        with (BENCHMARKS / "organisation-vocabulary-regression-v1.json").open(
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


if __name__ == "__main__":
    unittest.main()
