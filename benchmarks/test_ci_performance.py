"""Unit tests for the registered CI performance orchestrator."""

from __future__ import annotations

import importlib.util
import json
import pathlib
import tempfile
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("ci_performance.py")
SPEC = importlib.util.spec_from_file_location("lectern_ci_performance", MODULE_PATH)
assert SPEC and SPEC.loader
CI_PERFORMANCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CI_PERFORMANCE)


class CiPerformanceTests(unittest.TestCase):
    def test_checked_in_registry_is_valid(self) -> None:
        registry = CI_PERFORMANCE.load_registry(CI_PERFORMANCE.DEFAULT_REGISTRY)

        self.assertEqual(
            [suite["name"] for suite in registry["suites"]],
            [
                "maintenance-v1",
                "query-full-v1",
                "query-full-covered-v1",
                "query-page-v1",
                "query-page-covered-v1",
                "remove-book-v1",
                "attach-format-v1",
                "detach-asset-v1",
                "replace-asset-v1",
                "export-asset-v1",
                "reimport-known-path-v1",
                "organisation-migration-v3",
                "organisation-query-v3",
                "library-browse-v1",
                "organisation-vocabulary-v2",
                "bulk-tags-v2",
                "bulk-remove-v2",
                "saved-searches-v2",
                "ui-bootstrap-v1",
                "ui-selection-v1",
                "ui-book-detail-v2",
            ],
        )
        self.assertIn(
            {
                "name": "reimport-known-path-v1",
                "budget": "benchmarks/reimport-known-path-regression-v1.json",
            },
            registry["suites"],
        )
        self.assertIn(
            {
                "name": "bulk-tags-v2",
                "budget": "benchmarks/bulk-tags-regression-v2.json",
            },
            registry["suites"],
        )
        self.assertIn(
            {
                "name": "saved-searches-v2",
                "budget": "benchmarks/saved-searches-regression-v2.json",
            },
            registry["suites"],
        )
        self.assertIn(
            {
                "name": "ui-bootstrap-v1",
                "budget": "benchmarks/ui-bootstrap-regression-v1.json",
            },
            registry["suites"],
        )
        self.assertIn(
            {
                "name": "ui-selection-v1",
                "budget": "benchmarks/ui-selection-regression-v1.json",
            },
            registry["suites"],
        )
        self.assertIn(
            {
                "name": "maintenance-v1",
                "budget": "benchmarks/maintenance-regression-v1.json",
            },
            registry["suites"],
        )

    def test_workflow_provides_a_display_for_compositor_suites(self) -> None:
        workflow = (
            MODULE_PATH.parents[1] / ".github" / "workflows" / "performance.yml"
        ).read_text(encoding="utf-8")

        self.assertEqual(workflow.count("xvfb-run --auto-servernum"), 2)

    def test_registry_rejects_unsafe_budget_path(self) -> None:
        registry = {
            "schema_version": 1,
            "kind": "lectern-performance-suite-registry",
            "suites": [{"name": "query-v1", "budget": "../outside.json"}],
        }

        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "registry.json"
            path.write_text(json.dumps(registry), encoding="utf-8")

            with self.assertRaisesRegex(
                CI_PERFORMANCE.OrchestrationError, "safe repository-relative"
            ):
                CI_PERFORMANCE.load_registry(path)

    def test_registry_rejects_duplicate_names(self) -> None:
        registry = {
            "schema_version": 1,
            "kind": "lectern-performance-suite-registry",
            "suites": [
                {"name": "query-v1", "budget": "benchmarks/one.json"},
                {"name": "query-v1", "budget": "benchmarks/two.json"},
            ],
        }

        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "registry.json"
            path.write_text(json.dumps(registry), encoding="utf-8")

            with self.assertRaisesRegex(CI_PERFORMANCE.OrchestrationError, "repeats name"):
                CI_PERFORMANCE.load_registry(path)

    def test_paired_run_count_is_strict(self) -> None:
        self.assertEqual(
            CI_PERFORMANCE.paired_run_count(
                {"comparison": {"paired_runs": 3}}
            ),
            3,
        )
        with self.assertRaisesRegex(CI_PERFORMANCE.OrchestrationError, "positive integer"):
            CI_PERFORMANCE.paired_run_count(
                {"comparison": {"paired_runs": True}}
            )


if __name__ == "__main__":
    unittest.main()
