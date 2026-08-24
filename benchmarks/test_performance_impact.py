"""Unit tests for performance-impact classification."""

from __future__ import annotations

import importlib.util
import pathlib
import sys
import tempfile
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("performance_impact.py")
SPEC = importlib.util.spec_from_file_location("lectern_performance_impact", MODULE_PATH)
assert SPEC and SPEC.loader
IMPACT = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = IMPACT
SPEC.loader.exec_module(IMPACT)


class PerformanceImpactTests(unittest.TestCase):
    def test_runtime_and_build_paths_require_benchmarks(self) -> None:
        classification = IMPACT.classify_paths(
            [
                "crates/lectern-storage/src/lib.rs",
                "Cargo.lock",
                ".cargo/config.toml",
            ]
        )

        self.assertTrue(classification.required)
        self.assertEqual(len(classification.sensitive), 3)

    def test_performance_policy_and_infrastructure_require_benchmarks(self) -> None:
        classification = IMPACT.classify_paths(
            [
                "AGENTS.md",
                "docs/performance-policy.md",
                ".github/workflows/performance.yml",
                "benchmarks/query-regression-v1.json",
            ]
        )

        self.assertTrue(classification.required)
        self.assertEqual(len(classification.sensitive), 4)

    def test_documentation_only_change_is_exempt(self) -> None:
        classification = IMPACT.classify_paths(
            ["README.md", "docs/architecture.md", ".github/pull_request_template.md"]
        )

        self.assertFalse(classification.required)
        self.assertFalse(classification.sensitive)
        self.assertEqual(len(classification.exempt), 3)

    def test_paths_are_normalized_deduplicated_and_sorted(self) -> None:
        classification = IMPACT.classify_paths(
            ["./crates/z/src/lib.rs", "crates\\a\\src\\lib.rs", "crates/z/src/lib.rs"]
        )

        self.assertEqual(
            classification.sensitive,
            ("crates/a/src/lib.rs", "crates/z/src/lib.rs"),
        )

    def test_github_output_is_appended(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory) / "github-output"

            IMPACT.append_github_output(output, True)

            self.assertEqual(output.read_text(encoding="utf-8"), "required=true\n")

    def test_runtime_change_cannot_declare_no_impact(self) -> None:
        body = "- [x] None — cannot affect runtime work\n"

        with self.assertRaisesRegex(RuntimeError, "requires Potential or Material"):
            IMPACT.validate_pull_request_declaration(body, True)

    def test_sensitive_declaration_requires_all_acknowledgements(self) -> None:
        body = "- [x] Potential — may affect a measured path\n"

        with self.assertRaisesRegex(RuntimeError, "must acknowledge"):
            IMPACT.validate_pull_request_declaration(body, False)

    def test_material_declaration_with_evidence_is_accepted(self) -> None:
        lines = [
            "- [x] Material — changes or adds a hot path",
            *[
                f"- [x] {acknowledgement}"
                for acknowledgement in IMPACT.REQUIRED_ACKNOWLEDGEMENTS
            ],
        ]

        declaration = IMPACT.validate_pull_request_declaration(
            "\n".join(lines), True
        )

        self.assertEqual(declaration, "Material")

    def test_exactly_one_declaration_is_required(self) -> None:
        body = "- [x] None\n- [x] Potential\n"

        with self.assertRaisesRegex(RuntimeError, "exactly one"):
            IMPACT.validate_pull_request_declaration(body, False)


if __name__ == "__main__":
    unittest.main()
