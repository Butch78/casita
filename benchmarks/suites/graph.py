#!/usr/bin/env python3
"""Measure forced-spill graph verification and collection.

This is deliberately separate from the filesystem corpus benchmark: it keeps
the graph shape and spill limits explicit, so its results answer whether graph
work remains bounded when retained state is far larger than the in-memory
frontier. Setup is outside each timed sample; the command itself is measured
with the same wait4 timer as the repository benchmark suite.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import pathlib
import platform
import re
import shutil
import subprocess
import sys
import tempfile
from typing import Any

from benchmarks.suites.repository import CommandSpec, measured_command


PROFILES = {
    "smoke": 256,
    "standard": 2_048,
    "frontier": 8_192,
}


def command(executable: str, repository: pathlib.Path, *arguments: str) -> list[str]:
    return [executable, "--repository", str(repository), *arguments]


def checked(arguments: list[str]) -> None:
    completed = subprocess.run(arguments, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if completed.returncode:
        raise RuntimeError(
            f"command failed ({completed.returncode}): {' '.join(arguments)}\n{completed.stderr}"
        )


def output(arguments: list[str]) -> str:
    completed = subprocess.run(arguments, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if completed.returncode:
        return f"unavailable ({completed.stderr.strip()})"
    return completed.stdout.strip()


def environment(executable: str) -> dict[str, Any]:
    git_status = output(["git", "status", "--porcelain"])
    return {
        "casita_version": output([executable, "--version"]),
        "casita_revision": output(["git", "rev-parse", "HEAD"]),
        "casita_worktree_dirty": bool(git_status),
        "git_status": git_status,
        "captured_at_utc": dt.datetime.now(dt.UTC).isoformat(),
        "platform": platform.platform(),
        "python": platform.python_version(),
    }


def source_tree(root: pathlib.Path, objects: int, prefix: str, *, branches: int = 64) -> None:
    for index in range(objects):
        directory = root / f"branch-{index % branches:02d}"
        directory.mkdir(parents=True, exist_ok=True)
        # Unique content prevents payload deduplication from hiding the object
        # count; grouping creates a wide, shared directory frontier.
        (directory / f"node-{index:08d}").write_text(f"{prefix} object {index}\n", encoding="utf-8")


def setup(executable: str, workspace: pathlib.Path, objects: int, *, branches: int = 64) -> pathlib.Path:
    repository = workspace / "repository"
    retained = workspace / "retained"
    source_tree(retained, objects, "retained", branches=branches)
    checked(command(executable, repository, "init"))
    checked(command(executable, repository, "import", str(retained), "--root", "bench/retained"))
    # Collection still marks the complete live inventory and derives its
    # payload/chunk sets. Keeping this setup root-only avoids charging the
    # traversal benchmark for unrelated root mutation and WAL coordination.
    return repository


def active_spill_files(repository: pathlib.Path) -> list[pathlib.Path]:
    spill = repository / "spill"
    if not spill.is_dir():
        return []
    return sorted(spill.iterdir())


def spill_metrics(stdout: pathlib.Path, expectation: str = "spill") -> dict[str, int]:
    match = re.search(
        r"traversal-spill-files-opened (\d+); traversal-spill-peak-bytes (\d+)",
        stdout.read_text(encoding="utf-8"),
    )
    if match is None:
        raise RuntimeError("timed command did not report traversal spill metrics")
    files_opened, peak_bytes = map(int, match.groups())
    if expectation == "spill" and (files_opened == 0 or peak_bytes == 0):
        raise RuntimeError(
            f"forced-spill workload did not spill: files_opened={files_opened}, peak_bytes={peak_bytes}"
        )
    if expectation == "memory" and (files_opened or peak_bytes):
        raise RuntimeError("in-memory control unexpectedly spilled")
    return {"spill_files_opened": files_opened, "spill_peak_bytes": peak_bytes}


def sample(
    executable: str,
    root: pathlib.Path,
    objects: int,
    memory_objects: int,
    spill_bytes: int,
    operation: str,
    repetition: int,
    fsck_mode: str,
    expectation: str = "spill",
) -> dict[str, Any]:
    workspace = root / f"{operation}-{memory_objects}-{repetition}"
    repository = setup(executable, workspace, objects)
    action = "fsck" if operation == "verify-closure" else "gc"
    invocation = command(
        executable,
        repository,
        "--spill-memory-objects",
        str(memory_objects),
        "--spill-bytes",
        str(spill_bytes),
        action,
    )
    if operation == "verify-closure":
        invocation.append(f"--{fsck_mode}")
    stdout = workspace / "timed.stdout"
    stderr = workspace / "timed.stderr"
    timing = measured_command(
        CommandSpec(steps=[invocation], cwd=workspace, env={**os.environ, "LC_ALL": "C", "TZ": "UTC"}),
        stdout,
        stderr,
    )
    leftovers = active_spill_files(repository)
    if leftovers:
        raise RuntimeError(f"{operation} left spill files behind: {leftovers}")
    return {
        "status": "ok",
        "implementation": "casita",
        "workload": "wide-tree",
        "cache_policy": expectation,
        "operation": operation,
        "repetition": repetition,
        "objects": objects,
        "spill_memory_objects": memory_objects,
        "spill_bytes_budget": spill_bytes,
        "spill_cleanup": "ok",
        **spill_metrics(stdout, expectation),
        **timing,
    }


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--casita", default=shutil.which("casita") or "casita")
    parser.add_argument("--profile", choices=sorted(PROFILES), default="smoke")
    parser.add_argument("--objects", type=int, help="override the profile object count")
    parser.add_argument("--spill-memory-objects", type=int, default=1024)
    parser.add_argument("--spill-expectation", choices=("spill", "memory", "either"), default="either")
    parser.add_argument("--spill-thresholds", help="comma-separated memory-object limits, including a no-spill control")
    parser.add_argument(
        "--spill-bytes",
        type=int,
        # 8,192 distinct keys need roughly 64 MiB per active SQLite spill
        # file.  Fsck carries several inventories, so this leaves room for a
        # successful standard point while still making the budget explicit.
        default=512 * 1024 * 1024,
    )
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument(
        "--casita-fsck-mode",
        choices=("audit-only", "dry-run"),
        default="audit-only",
        help="verification mode; dry-run supports older Casita revisions",
    )
    parser.add_argument("--output", type=pathlib.Path, required=True)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if args.objects is not None and args.objects < 1:
        raise SystemExit("--objects must be at least 1")
    if args.spill_memory_objects < 1:
        raise SystemExit("--spill-memory-objects must be at least 1")
    if args.repetitions < 1:
        raise SystemExit("--repetitions must be at least 1")
    objects = args.objects or PROFILES[args.profile]
    thresholds = [int(value) for value in args.spill_thresholds.split(",")] if args.spill_thresholds else [args.spill_memory_objects]
    if any(value < 1 for value in thresholds):
        raise SystemExit("spill thresholds must be positive")
    with tempfile.TemporaryDirectory(prefix="casita-graph-traversal-") as temporary:
        root = pathlib.Path(temporary)
        samples = [
            sample(
                args.casita,
                root,
                objects,
                threshold,
                args.spill_bytes,
                operation,
                repetition,
                args.casita_fsck_mode,
                args.spill_expectation,
            )
            for threshold in thresholds
            for repetition in range(1, args.repetitions + 1)
            for operation in ("verify-closure", "collect")
        ]
    result = {
        "schema_version": 1,
        "result_schema": "casita.graph-traversal.v1",
        "suite_id": "graph-traversal",
        "profile": args.profile,
        "environment": environment(args.casita),
        "configuration": {
            "profile": args.profile,
            "objects": objects,
            "spill_memory_objects": args.spill_memory_objects,
            "spill_thresholds": thresholds,
            "spill_expectation": args.spill_expectation,
            "spill_bytes": args.spill_bytes,
            "casita_fsck_mode": args.casita_fsck_mode,
        },
        "tools": {"casita": {"status": "available", "version": output([args.casita, "--version"])}},
        "notes": "Each timed command uses an explicit spill threshold; setup, import, and cleanup verification are outside timing.",
        "samples": samples,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"wrote {args.output}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except RuntimeError as error:
        print(f"graph traversal benchmark failed: {error}", file=sys.stderr)
        raise SystemExit(1)
