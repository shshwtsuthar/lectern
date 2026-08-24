"""Unit tests for the stdlib-only benchmark orchestrator."""

from __future__ import annotations

import importlib.util
import pathlib
import tempfile
import unittest
from unittest import mock


MODULE_PATH = pathlib.Path(__file__).with_name("run.py")
SPEC = importlib.util.spec_from_file_location("lectern_benchmark_run", MODULE_PATH)
assert SPEC and SPEC.loader
RUN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUN)


class RunnerTests(unittest.TestCase):
    def test_nearest_rank_summary_retains_observed_values(self) -> None:
        samples = list(range(1, 101))

        summary = RUN.summarize(samples)

        self.assertEqual(summary["p50"], 50)
        self.assertEqual(summary["p95"], 95)
        self.assertEqual(summary["p99"], 99)

    def test_inventory_counts_only_supported_publications(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "book.epub").write_bytes(b"epub")
            (root / "manual.PDF").write_bytes(b"pdf-data")
            (root / "notes.txt").write_text("ignored", encoding="utf-8")

            inventory = RUN.inventory_corpus(root)

        self.assertEqual(inventory["files"], 2)
        self.assertEqual(inventory["epub_files"], 1)
        self.assertEqual(inventory["pdf_files"], 1)
        self.assertEqual(inventory["total_bytes"], 12)

    def test_adjacent_corpus_manifest_prefers_reproducible_recipe(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            corpus = root / "corpus"
            corpus.mkdir()
            (root / "corpus_stats.json").write_text(
                '{"kind":"legacy"}', encoding="utf-8"
            )
            (root / "corpus-manifest.json").write_text(
                '{"kind":"pinned"}', encoding="utf-8"
            )

            manifest = RUN.adjacent_corpus_manifest(corpus)

        self.assertEqual(manifest, {"kind": "pinned"})

    def test_smoke_mode_bounds_expensive_settings(self) -> None:
        options = RUN.parse_arguments(["--smoke", "--books", "50000"])

        self.assertEqual(options.books, 1000)
        self.assertEqual(options.query_iterations, 5)
        self.assertEqual(options.startup_runs, 1)
        self.assertEqual(options.sort_iterations, 3)
        self.assertEqual(options.asset_action_iterations, 3)
        self.assertEqual(options.editor_warmup_iterations, 1)
        self.assertEqual(options.editor_iterations, 3)

    def test_command_timeout_is_recorded_before_failing(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            recorder = RUN.CommandRecorder(root, root / "commands.json")
            expired = RUN.subprocess.TimeoutExpired(["lectern"], 2.0)
            with mock.patch.object(RUN.subprocess, "run", side_effect=expired):
                with self.assertRaisesRegex(RuntimeError, "timed out after 2.0 seconds"):
                    recorder.run(["lectern"], timeout_seconds=2.0)
            command_log = RUN.read_json(root / "commands.json")

        self.assertTrue(command_log["commands"][0]["timed_out"])
        self.assertIsNone(command_log["commands"][0]["return_code"])

    def test_desktop_timeout_includes_grace_period(self) -> None:
        calls = []

        class Recorder:
            def run(self, command, **options):
                calls.append((command, options))

        RUN.run_desktop(
            Recorder(),
            pathlib.Path("lectern"),
            pathlib.Path("library"),
            pathlib.Path("result.json"),
            idle_seconds=3.0,
            scroll_seconds=15.0,
            scroll_warmup_seconds=1.0,
            scroll_pixels_per_second=1_500.0,
            sort_iterations=40,
            asset_action_iterations=30,
            editor_warmup_iterations=10,
            editor_iterations=20,
            timeout_seconds=20.0,
        )

        self.assertEqual(
            calls[0][1]["timeout_seconds"],
            20.0 + RUN.DESKTOP_TIMEOUT_GRACE_SECONDS,
        )
        self.assertEqual(
            calls[0][1]["environment"]["LECTERN_BENCHMARK_SORT_ITERATIONS"],
            "40",
        )
        self.assertEqual(
            calls[0][1]["environment"]["LECTERN_BENCHMARK_ASSET_ACTION_ITERATIONS"],
            "30",
        )
        self.assertEqual(
            calls[0][1]["environment"]["LECTERN_BENCHMARK_EDITOR_ITERATIONS"],
            "20",
        )

    def test_integrity_validators_accept_reconciled_counts(self) -> None:
        RUN.validate_seed_result(
            {"requested_books": 50_000, "stored_books": 50_000}, 50_000
        )
        RUN.validate_desktop_result(
            {
                "library": {"books": 50_000},
                "startup": {"main_entry_to_populated_library_ns": 42},
            },
            50_000,
        )
        RUN.validate_desktop_result(
            {
                "library": {"books": 50_000},
                "startup": {"main_entry_to_populated_library_ns": 42},
                "sort_interactions": {
                    "iterations_per_sort": 2,
                    "scenarios": [
                        {
                            "name": name,
                            "first_book_id": index + 1,
                            "samples_ns": [16_000_000, 17_000_000],
                            "passed": True,
                        }
                        for index, name in enumerate(
                            ("title", "author", "recently_added")
                        )
                    ],
                },
                "asset_actions": {
                    "iterations_per_action": 2,
                    "scenarios": [
                        {
                            "name": name,
                            "samples_ns": [8_000_000, 9_000_000],
                            "passed": True,
                        }
                        for name in ("open", "reveal")
                    ],
                },
                "editor_interactions": {
                    "warmup_iterations": 1,
                    "measured_iterations": 2,
                    "book_id": 1,
                    "samples_ns": [12_000_000, 13_000_000],
                    "passed": True,
                },
            },
            50_000,
            2,
            2,
            1,
            2,
        )
        RUN.validate_query_result(
            {
                "library_books": 50_000,
                "measured_iterations": 2,
                "scenarios": [
                    {
                        "name": "sort_title",
                        "result_count": 50_000,
                        "samples_ns": [10, 11],
                    },
                    {
                        "name": "search_title_prefix",
                        "result_count": 4_000,
                        "samples_ns": [3, 4],
                    },
                ],
            },
            50_000,
            2,
        )
        RUN.validate_import_result(
            {
                "corpus": {"files": 10_000},
                "summary": {"discovered": 10_000, "imported": 9_999, "failed": 1},
                "database_books": 9_999,
            },
            10_000,
        )

    def test_integrity_validators_reject_mismatched_counts(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "seed count mismatch"):
            RUN.validate_seed_result(
                {"requested_books": 50_000, "stored_books": 49_999}, 50_000
            )
        with self.assertRaisesRegex(RuntimeError, "desktop library count mismatch"):
            RUN.validate_desktop_result(
                {
                    "library": {"books": 49_999},
                    "startup": {"main_entry_to_populated_library_ns": 42},
                },
                50_000,
            )
        with self.assertRaisesRegex(RuntimeError, "sample count mismatch"):
            RUN.validate_query_result(
                {
                    "library_books": 50_000,
                    "measured_iterations": 2,
                    "scenarios": [
                        {
                            "name": "sort_title",
                            "result_count": 50_000,
                            "samples_ns": [10],
                        }
                    ],
                },
                50_000,
                2,
            )
        with self.assertRaisesRegex(RuntimeError, "does not reconcile"):
            RUN.validate_import_result(
                {
                    "corpus": {"files": 10_000},
                    "summary": {
                        "discovered": 10_000,
                        "imported": 9_998,
                        "failed": 1,
                    },
                    "database_books": 9_998,
                },
                10_000,
            )


if __name__ == "__main__":
    unittest.main()
