#!/usr/bin/env python3
"""Classify a Git diff for Lectern's mandatory performance gate."""

from __future__ import annotations

import argparse
import pathlib
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


def append_github_output(path: pathlib.Path, classification: Classification) -> None:
    """Publish the stable output consumed by the performance workflow."""

    with path.open("a", encoding="utf-8") as output:
        output.write(f"required={'true' if classification.required else 'false'}\n")


def report(classification: Classification) -> None:
    decision = "required" if classification.required else "not required"
    print(f"Performance benchmark: {decision}")
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
    report(classification)
    if options.github_output is not None:
        append_github_output(options.github_output, classification)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv[1:]))
    except (OSError, RuntimeError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(2) from error
