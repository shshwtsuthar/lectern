#!/usr/bin/env python3
"""Classify a Git diff for Lectern's mandatory performance gate."""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import subprocess
import sys
from dataclasses import dataclass
from typing import Iterable


REPOSITORY = pathlib.Path(__file__).resolve().parents[1]
SENSITIVE_FILES = frozenset(
    {
        ".github/workflows/performance.yml",
        "AGENTS.md",
        "Cargo.lock",
        "Cargo.toml",
        "docs/performance-policy.md",
        "rust-toolchain.toml",
    }
)
SENSITIVE_PREFIXES = (".cargo/", "benchmarks/", "crates/")
CLASSIFICATION_PATTERN = re.compile(
    r"^\s*-\s*\[[xX]\]\s*(None|Potential|Material)\b", re.MULTILINE
)
REQUIRED_ACKNOWLEDGEMENTS = (
    "Applicable deterministic scenario and budget added or updated",
    "Candidate passes applicable absolute and relative regression budgets",
    "No benchmark workload or budget was weakened to obtain a pass",
)


@dataclass(frozen=True)
class Classification:
    """Performance impact of a set of changed repository paths."""

    sensitive: tuple[str, ...]
    exempt: tuple[str, ...]

    @property
    def required(self) -> bool:
        """Whether the performance benchmark job must run."""

        return bool(self.sensitive)


def parse_arguments(arguments: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Classify changed files for Lectern's performance gate."
    )
    parser.add_argument("--base", required=True, help="Base Git revision")
    parser.add_argument("--head", default="HEAD", help="Candidate Git revision")
    parser.add_argument(
        "--github-output",
        type=pathlib.Path,
        help="Append the required=true/false output for GitHub Actions",
    )
    parser.add_argument(
        "--github-event",
        type=pathlib.Path,
        help="Validate the pull-request declaration in this GitHub event payload",
    )
    return parser.parse_args(arguments)


def normalize_path(path: str) -> str:
    """Return a stable repository-relative path representation."""

    return path.replace("\\", "/").removeprefix("./")


def is_performance_sensitive(path: str) -> bool:
    """Conservatively identify files capable of changing runtime performance."""

    normalized = normalize_path(path)
    return normalized in SENSITIVE_FILES or normalized.startswith(SENSITIVE_PREFIXES)


def classify_paths(paths: Iterable[str]) -> Classification:
    """Partition changed paths into performance-sensitive and exempt sets."""

    normalized = sorted({normalize_path(path) for path in paths if path})
    sensitive = tuple(path for path in normalized if is_performance_sensitive(path))
    exempt = tuple(path for path in normalized if not is_performance_sensitive(path))
    return Classification(sensitive=sensitive, exempt=exempt)


def changed_paths(base: str, head: str, repository: pathlib.Path = REPOSITORY) -> list[str]:
    """Read changed paths from a three-dot Git comparison without shell parsing."""

    command = [
        "git",
        "diff",
        "--name-only",
        "--diff-filter=ACMRTUXB",
        "-z",
        f"{base}...{head}",
        "--",
    ]
    result = subprocess.run(
        command,
        cwd=repository,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", errors="replace").strip()
        raise RuntimeError(
            f"Git diff failed for {base!r}...{head!r} with exit code "
            f"{result.returncode}: {detail}"
        )
    return [
        value.decode("utf-8", errors="surrogateescape")
        for value in result.stdout.split(b"\0")
        if value
    ]


def validate_pull_request_declaration(body: str, automated_required: bool) -> str:
    """Validate and return the exactly-one performance impact declaration."""

    selected = CLASSIFICATION_PATTERN.findall(body)
    if len(selected) != 1:
        raise RuntimeError(
            "pull request must select exactly one performance impact: "
            "None, Potential, or Material"
        )
    declaration = selected[0]
    if automated_required and declaration == "None":
        raise RuntimeError(
            "automated path classification requires Potential or Material performance impact"
        )
    if declaration != "None":
        for acknowledgement in REQUIRED_ACKNOWLEDGEMENTS:
            pattern = re.compile(
                rf"^\s*-\s*\[[xX]\]\s*{re.escape(acknowledgement)}\s*$",
                re.MULTILINE,
            )
            if pattern.search(body) is None:
                raise RuntimeError(
                    f"pull request must acknowledge: {acknowledgement}"
                )
    return declaration


def declaration_from_event(path: pathlib.Path, automated_required: bool) -> str:
    """Read and validate a GitHub pull-request body from its event payload."""

    with path.open(encoding="utf-8") as source:
        event = json.load(source)
    if not isinstance(event, dict) or not isinstance(event.get("pull_request"), dict):
        raise RuntimeError("GitHub event does not contain a pull request")
    body = event["pull_request"].get("body")
    if body is None:
        body = ""
    if not isinstance(body, str):
        raise RuntimeError("GitHub pull-request body must be text")
    return validate_pull_request_declaration(body, automated_required)


def append_github_output(path: pathlib.Path, required: bool) -> None:
    """Publish the stable output consumed by the performance workflow."""

    with path.open("a", encoding="utf-8") as output:
        output.write(f"required={'true' if required else 'false'}\n")


def report(classification: Classification, required: bool, declaration: str | None) -> None:
    decision = "required" if required else "not required"
    print(f"Performance benchmark: {decision}")
    if declaration is not None:
        print(f"Pull-request declaration: {declaration}")
    if classification.sensitive:
        print("Sensitive paths:")
        for path in classification.sensitive:
            print(f"  {path}")
    if classification.exempt:
        print("Exempt paths:")
        for path in classification.exempt:
            print(f"  {path}")


def main(arguments: list[str]) -> int:
    options = parse_arguments(arguments)
    classification = classify_paths(changed_paths(options.base, options.head))
    declaration = (
        declaration_from_event(options.github_event, classification.required)
        if options.github_event is not None
        else None
    )
    required = classification.required or declaration in ("Potential", "Material")
    report(classification, required, declaration)
    if options.github_output is not None:
        append_github_output(options.github_output, required)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv[1:]))
    except (OSError, RuntimeError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(2) from error
