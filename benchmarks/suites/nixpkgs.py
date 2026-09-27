#!/usr/bin/env python3
"""Run the repository acceptance suite against a committed nixpkgs tree.

This is a thin policy entrypoint over the unified repository runner. It keeps
the raw schema, cache controls, timing, throughput, storage accounting,
validation, reports, and dashboard normalization identical to the synthetic
repository workloads.
"""

from __future__ import annotations

import datetime as dt
import pathlib
import sys
from collections.abc import Sequence

from benchmarks.suites import repository


DEFAULT_OPERATIONS = "cold-import,unchanged-import,checkout,verify"
DEFAULT_IMPLEMENTATIONS = "casita,git"


def has_option(arguments: Sequence[str], name: str) -> bool:
    return any(argument == name or argument.startswith(name + "=") for argument in arguments)


def main(argv: Sequence[str] | None = None) -> int:
    arguments = list(sys.argv[1:] if argv is None else argv)
    defaults = [
        "--profile",
        "standard",
        "--corpora",
        "nixpkgs",
        "--operations",
        DEFAULT_OPERATIONS,
        "--implementations",
        DEFAULT_IMPLEMENTATIONS,
        "--cache-policies",
        "warm,cold",
        "--repetitions",
        "10",
        "--git-pack-comparator",
    ]
    if not has_option(arguments, "--output"):
        timestamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        defaults.extend(
            [
                "--output",
                str(pathlib.Path("benchmarks/results") / f"nixpkgs-{timestamp}.json"),
            ]
        )
    return repository.main([*defaults, *arguments])


if __name__ == "__main__":
    raise SystemExit(main())
