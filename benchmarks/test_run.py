"""Unit tests for the stdlib-only benchmark orchestrator."""

from __future__ import annotations

import importlib.util
import pathlib
import tempfile
import unittest


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

    def test_smoke_mode_bounds_expensive_settings(self) -> None:
        options = RUN.parse_arguments(["--smoke", "--books", "50000"])

        self.assertEqual(options.books, 1000)
        self.assertEqual(options.query_iterations, 5)
        self.assertEqual(options.startup_runs, 1)


if __name__ == "__main__":
    unittest.main()
