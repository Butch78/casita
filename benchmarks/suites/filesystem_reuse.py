"""Filesystem import reuse, directory staging, and durable pin-journal profiling."""
from __future__ import annotations

import argparse
from collections import defaultdict
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import time

from benchmarks import cli

PROFILES = {
    "smoke": [1, 14, 15, 16, 17, 64],
    # Each child contributes a file and a directory. Include both sides of
    # the 16-write window, 1024-entry walk page, and 4096-object publication
    # batch (both cached and forced-reread imports).
    "standard": [1, 14, 15, 16, 17, 64, 511, 512, 513, 1024,
                 2047, 2048, 2049, 4095, 4096, 4097],
}


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def inventory(root):
    result = {}
    for path in root.rglob("*"):
        name = str(path.relative_to(root))
        if path.is_symlink():
            result[name] = ["link", os.readlink(path)]
        elif path.is_file():
            result[name] = ["file", digest(path), bool(path.stat().st_mode & 0o111)]
        elif path.is_dir():
            result[name] = ["directory"]
    return result


def trace_summary(path):
    spans, phases = defaultdict(lambda: [0, 0.0]), defaultdict(lambda: [0, 0.0])
    hits = misses = 0
    for line in path.read_text().splitlines():
        event = json.loads(line)
        fields = event["fields"]
        if event["target"] == "casita::pin_timing" and "elapsed_seconds" in fields:
            value = phases[fields["phase"]]
            value[0] += 1
            value[1] += fields["elapsed_seconds"]
        if fields.get("message") == "close":
            value = spans[event["span"]["name"]]
            value[0] += 1
            for key in ("time.busy", "time.idle"):
                number, unit = re.fullmatch(r"([\d.]+)(s|ms|µs|ns)", fields[key]).groups()
                value[1] += float(number) * {"s": 1, "ms": 1e-3, "µs": 1e-6, "ns": 1e-9}[unit]
        if event["target"] == "casita::filesystem::cache":
            hits += fields.get("hits", 0)
            misses += fields.get("misses", 0)
    if spans["repository.import_path.walk"][0] != 1:
        raise ValueError("trace must contain exactly one completed filesystem import")
    return {"cache_hits": hits, "cache_misses": misses,
            "spans": dict(spans), "pin_phases": dict(phases)}


def render_report(result):
    lines = ["# Filesystem reuse", "", f"Complete: {result['complete']}", "",
             "Span and phase times overlap; do not add them. Untraced rows have no trace counters.", "",
             "| Operation | Directories | Wall seconds | Cache hits/misses | Directory staging seconds | Journal flushes | Flush seconds |",
             "|---|---:|---:|---:|---:|---:|---:|"]
    for sample in result["samples"]:
        trace = sample.get("trace")
        detail = "n/a | n/a | n/a | n/a"
        if trace:
            directories = trace["spans"].get("repository.stage_directory", [0, 0])[1]
            flushes, elapsed = trace["pin_phases"].get("journal_flush", [0, 0])
            detail = f"{trace['cache_hits']}/{trace['cache_misses']} | {directories:.3f} | {flushes} | {elapsed:.3f}"
        lines.append(f"| {sample['operation']} | {sample['directories']} | {sample['wall_seconds']:.3f} | {detail} |")
    if "error" in result:
        lines += ["", "Error: " + result["error"]]
    return "\n".join(lines) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="smoke")
    parser.add_argument("--directories", help="comma-separated generated directory counts")
    parser.add_argument("--source", type=Path, help="read-only existing tree instead of generated fixtures")
    parser.add_argument("--repository", type=Path, help="existing repository; benchmark roots are retained under a unique prefix")
    parser.add_argument("--casita-bin", type=Path, default=cli.ROOT / "target/debug/casita")
    parser.add_argument("--repetitions", type=int, default=1)
    parser.add_argument("--no-build", action="store_true", help="always uses a prebuilt binary")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args(argv)
    counts = [int(n) for n in args.directories.split(",")] if args.directories else PROFILES[args.profile]
    if args.repetitions < 1 or any(n < 1 for n in counts) or not args.casita_bin.is_file():
        parser.error("positive counts/repetitions and a prebuilt Casita binary are required")
    if args.source and not args.source.is_dir():
        parser.error("--source must be a directory")
    if args.source and (args.output.resolve().parent.is_relative_to(args.source.resolve())
                        or args.repository and args.repository.resolve().is_relative_to(args.source.resolve())):
        parser.error("output and repository must be outside the source tree")
    args.output = args.output.resolve()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="filesystem-reuse-", dir=args.output.parent))
    binary = work / "casita"
    shutil.copy2(args.casita_bin, binary)
    result = {"schema_version": 1, "result_schema": "casita.filesystem-reuse.v1",
              "suite_id": "state-and-publication", "complete": False, "work": str(work),
              "configuration": {"directories": counts, "source": str(args.source) if args.source else None,
                                "repetitions": args.repetitions, "trace_filter": "casita=debug"},
              "artifacts": [{"path": str(binary), "sha256": digest(binary)}], "samples": []}

    def save():
        args.output.write_text(json.dumps(result, indent=2) + "\n")
        report = args.report or args.output.with_suffix(".md")
        report.write_text(render_report(result))

    save()
    try:
        for count in ([None] if args.source else counts):
            case = work / str(count)
            case.mkdir()
            source = args.source.resolve() if args.source else case / "source"
            if not args.source:
                for index in range(count):
                    directory = source / f"directory-{index:04}"
                    directory.mkdir(parents=True)
                    (directory / "file").write_bytes(hashlib.sha256(str(index).encode()).digest() * 2)
            repository = args.repository.resolve() if args.repository else case / "repository"
            root = f"benchmark/filesystem-reuse/{work.name}/{count}"
            expected = inventory(source)
            directories = 1 + sum(value[0] == "directory" for value in expected.values())

            def run(operation, traced=False, rehash=False):
                command = [str(binary), "--repository", str(repository), "--log-format", "json",
                           "--log-filter", "casita=debug" if traced else "off", "import", str(source),
                           "--importer", "filesystem", "--root", root]
                if rehash:
                    command.append("--filesystem-rehash")
                log = work / f"{count}-{len(result['samples'])}-{operation}"
                started = time.monotonic()
                load = os.getloadavg() if hasattr(os, "getloadavg") else None
                with log.with_suffix(".out").open("w") as out, log.with_suffix(".jsonl").open("w") as err:
                    process = subprocess.run(command, stdout=out, stderr=err)
                elapsed = time.monotonic() - started
                output = log.with_suffix(".out").read_text().splitlines()
                key = output[0] if output else ""
                okay = process.returncode == 0 and key.startswith("casita.directory.v1:")
                sample = {"operation": operation, "directories": directories, "wall_seconds": elapsed,
                          "load_average": load, "command": command, "log": str(log.with_suffix('.jsonl')),
                          "key": key, "status": "ok" if okay else "failed"}
                result["samples"].append(sample)
                save()
                if not okay:
                    raise RuntimeError(f"import failed: {log}")
                if traced:
                    sample["trace"] = trace_summary(log.with_suffix(".jsonl"))
                    if operation == "warm-traced" and os.name == "posix":
                        trace = sample["trace"]
                        files = sum(value[0] == "file" for value in expected.values())
                        if trace["cache_hits"] != files or trace["cache_misses"] or trace["spans"].get("repository.stage_blob", [0])[0]:
                            raise RuntimeError("warm import must reuse every regular file without staging blobs")
                save()
                return key

            key = run("prime")
            for _ in range(args.repetitions):
                for operation, traced, rehash in [("warm", False, False), ("warm-traced", True, False), ("rehash-traced", True, True)]:
                    if run(operation, traced, rehash) != key:
                        raise RuntimeError("unchanged tree identity differs across reuse/rehash")
            if inventory(source) != expected:
                raise RuntimeError("source changed during profiling")
            restored = case / "restored"
            subprocess.run([str(binary), "--repository", str(repository), "checkout", key, str(restored), "--no-root"], check=True, capture_output=True)
            if inventory(restored) != expected:
                raise RuntimeError("restored tree differs in bytes, paths, executable bits, or symlinks")
            result["samples"][-1]["verified_entries"] = len(expected)
            save()
        result["complete"] = True
    except Exception as error:
        result["error"] = str(error)
        if result["samples"]:
            result["samples"][-1]["status"] = "failed"
        raise
    finally:
        save()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
