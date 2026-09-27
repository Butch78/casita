#!/usr/bin/env python3
"""Discover and run Casita benchmark suites from one command."""

from __future__ import annotations

import importlib
import json
import pathlib
import subprocess
import sys
from collections.abc import Sequence
from typing import Any


ROOT = pathlib.Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "benchmarks" / "manifest.json"


class BenchmarkCliError(RuntimeError):
    pass


def entrypoints() -> list[dict[str, Any]]:
    manifest = json.loads(MANIFEST.read_text())
    entries = manifest.get("entrypoints")
    if not isinstance(entries, list):
        raise BenchmarkCliError("benchmark manifest has no entrypoints array")
    identifiers: set[str] = set()
    suite_ids = {suite["id"] for suite in manifest["suites"]}
    for entry in entries:
        identifier = entry.get("id")
        if not isinstance(identifier, str) or not identifier or identifier in identifiers:
            raise BenchmarkCliError(f"invalid or duplicate benchmark entrypoint: {identifier!r}")
        if entry.get("suite_id") not in suite_ids:
            raise BenchmarkCliError(
                f"entrypoint {identifier} names unknown suite {entry.get('suite_id')!r}"
            )
        kind = entry.get("kind")
        target = entry.get("target")
        if kind not in {"module", "command"} or not target:
            raise BenchmarkCliError(f"entrypoint {identifier} has an invalid target")
        identifiers.add(identifier)
    return entries


def render_list(entries: Sequence[dict[str, Any]]) -> str:
    width = max(len(entry["id"]) for entry in entries)
    lines = ["Available benchmark suites:", ""]
    for entry in entries:
        profiles = ",".join(entry.get("profiles", [])) or "custom"
        lines.append(f"  {entry['id']:<{width}}  {profiles:<25}  {entry['title']}")
    lines.extend(
        [
            "",
            "Run one with: benchmark run <suite> [suite options]",
            "Legacy `benchmark --profile ...` arguments run the repository suite.",
        ]
    )
    return "\n".join(lines)


def run_entrypoint(entry: dict[str, Any], arguments: Sequence[str]) -> int:
    arguments = [*entry.get("default_arguments", []), *arguments]
    if entry["kind"] == "module":
        module = importlib.import_module(entry["target"])
        main = getattr(module, "main", None)
        if main is None:
            raise BenchmarkCliError(f"benchmark module {entry['target']} has no main function")
        previous_program = sys.argv[0]
        sys.argv[0] = f"benchmark run {entry['id']}"
        try:
            return int(main(list(arguments)))
        finally:
            sys.argv[0] = previous_program
    command = [*entry["target"], *arguments]
    return subprocess.run(command, cwd=ROOT, check=False).returncode


def usage() -> str:
    return """usage: benchmark list [--json|--groups]
       benchmark run <suite> [suite options]
       benchmark all [--output DIRECTORY] [--profile smoke|standard] [--groups GROUP,...]
       benchmark inventory [--output FILE]
       benchmark archive DIRECTORY --output ARCHIVE.tar.gz
       benchmark clean [--apply]
       benchmark revisions REVISION REVISION [REVISION ...] [options] -- [suite options]
       benchmark export-bencher --result RESULT [--result RESULT ...]
       benchmark compare --base-result RESULT --head-result RESULT
       benchmark [repository suite options]

The final form preserves the original repository benchmark command.
See benchmarks/OPERATIONS.md for storage, archival and cleanup."""


def main(argv: Sequence[str] | None = None) -> int:
    arguments = list(sys.argv[1:] if argv is None else argv)
    try:
        entries = entrypoints()
        by_id = {entry["id"]: entry for entry in entries}
        if arguments[:1] == ["list"]:
            if arguments[1:] == ["--groups"]:
                for group in sorted({entry["suite_id"] for entry in entries}):
                    print(f"{group}: " + ", ".join(entry["id"] for entry in entries if entry["suite_id"] == group))
            elif arguments[1:] == ["--json"]:
                print(json.dumps(entries, indent=2, sort_keys=True))
            elif arguments[1:]:
                raise BenchmarkCliError("benchmark list accepts --json or --groups")
            else:
                print(render_list(entries))
            return 0
        if arguments[:1] in (["inventory"], ["archive"], ["clean"]):
            from benchmarks import storage
            return storage.main(arguments)
        if arguments[:1] == ["all"]:
            from benchmarks import all as all_suites
            return all_suites.main(arguments[1:])
        if arguments[:1] == ["run"]:
            if len(arguments) < 2:
                raise BenchmarkCliError("benchmark run requires a suite name")
            identifier = arguments[1]
            entry = by_id.get(identifier)
            if entry is None:
                raise BenchmarkCliError(
                    f"unknown benchmark suite {identifier!r}; run `benchmark list`"
                )
            return run_entrypoint(entry, arguments[2:])
        if arguments[:1] == ["revisions"]:
            from benchmarks import revisions

            return revisions.main(arguments[1:])
        if arguments[:1] in (["export-bencher"], ["compare"]):
            from benchmarks import comparison

            return comparison.main(arguments)
        if arguments[:1] in (["-h"], ["--help"]):
            print(usage())
            print()
            print(render_list(entries))
            return 0
        return run_entrypoint(by_id["repository"], arguments)
    except BenchmarkCliError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
