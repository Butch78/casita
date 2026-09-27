"""End-to-end local GC with overlapping historical selected holds."""
from __future__ import annotations

import argparse
import hashlib
import itertools
import json
import os
import pathlib
import re
import subprocess
import tempfile

from benchmarks import cli
from benchmarks.suites import repository as common
from benchmarks.suites.metadata_collection import CARGO_ARGUMENTS, positive_csv
from benchmarks.suites.pack.catalog import parse_probe_binary

PROBE = "blob::pack::gc_benchmark::benchmark_held_catalog_gc"
CORRECTNESS = "distinct shared-base holds; exact logical removals; held streams readable; packs reclaimed after release"


def parse_sample(stdout, count, holds):
    rows = [json.loads(line.removeprefix("held_catalog_gc_sample "))
            for line in stdout.splitlines() if line.startswith("held_catalog_gc_sample ")]
    if len(rows) != 1 or "test result: ok. 1 passed; 0 failed;" not in stdout:
        raise common.BenchmarkError("expected one passing held-catalog-gc probe")
    row = rows[0]
    if (row.get("count") != count or row.get("holds") != holds
            or row.get("correctness") != CORRECTNESS
            or row.get("garbage_objects") != count + holds - 2
            or type(row.get("pack_bytes_before")) is not int or row["pack_bytes_before"] <= 0
            or type(row.get("pack_bytes_during")) is not int
            or row["pack_bytes_during"] < row["pack_bytes_before"]
            or row.get("historical_packs_preserved") is not True
            or row.get("pack_bytes_after_release") != 0
            or any(type(row.get(key)) not in (float, int) or not 0 < row[key] < float("inf")
                   for key in ("seconds", "release_seconds"))
            or not any(phase.get("phase") == "finish_payload_collection" for phase in row.get("phases", []))):
        raise common.BenchmarkError("invalid held-catalog-gc configuration or correctness gate")
    return row


def compaction_summary(phases):
    """Union wall intervals so two concurrent pack writes are not double-counted.

    Busy times for different phases may overlap; they must not be added together.
    End times are recorded when the tracing subscriber receives each event.
    """
    groups = {}
    for phase in phases:
        if phase["phase"].startswith("compact_pack"):
            end, seconds = phase["finished_seconds"], phase["seconds"]
            groups.setdefault(phase["phase"], []).append((end - seconds, end))
    result = {}
    for name, intervals in groups.items():
        busy, previous_end = 0.0, float("-inf")
        for start, end in sorted(intervals):
            busy += max(0.0, end - max(start, previous_end))
            previous_end = max(previous_end, end)
        result[name] = dict(calls=len(intervals), busy_seconds=busy,
                            summed_seconds=sum(end - start for start, end in intervals))
    return result


def sync_trace_summary(paths):
    """Full-process sync totals; concurrent syscall times are not wall time.

    Marker-directory totals exclude shared ancestors outside pack-replacements.
    Setup, held GC, released GC and fixture cleanup are all inside this scope.
    """
    groups = {}
    unparsed = 0
    artifacts = []
    marker_files = set()
    directory_syncs = []
    pattern = re.compile(r"^\d+\.\d+ (?:fsync|fdatasync)\(\d+<([^>]+)>\) = (-?\d+).*<([\d.]+)>$")
    for path in sorted(paths):
        raw = path.read_bytes()
        artifacts.append(dict(path=str(path), sha256=hashlib.sha256(raw).hexdigest(), bytes=len(raw)))
        for line in raw.decode().splitlines():
            if "fsync(" not in line and "fdatasync(" not in line:
                continue
            match = pattern.match(line)
            if not match:
                unparsed += 1
                continue
            location, result, seconds = match.groups()
            kind = "other"
            if "/pack-replacements/" in location or location.endswith("/pack-replacements"):
                kind = "marker_file" if "#" in location else "marker_directory"
            if kind == "marker_file":
                marker_files.add(str(pathlib.Path(location).parent))
            elif kind == "marker_directory":
                directory_syncs.append((location, result, float(seconds)))
            group = groups.setdefault(kind, dict(calls=0, failures=0, summed_seconds=0.0))
            group["calls"] += 1
            group["failures"] += int(result != "0")
            group["summed_seconds"] += float(seconds)
    leaf_syncs = [(result, seconds) for location, result, seconds in directory_syncs if location in marker_files]
    leaf = dict(calls=len(leaf_syncs), failures=sum(result != "0" for result, _ in leaf_syncs),
                summed_seconds=sum(seconds for _, seconds in leaf_syncs))
    return dict(scope="full probe process including setup and both collections", groups=groups,
                marker_leaf_directory_subset=leaf,
                unparsed_sync_lines=unparsed, artifacts=artifacts)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--counts", type=positive_csv)
    parser.add_argument("--holds", type=positive_csv, default=[1, 8, 10])
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--probe-binary", type=pathlib.Path)
    parser.add_argument("--baseline-binary", type=pathlib.Path)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--strace-dir", type=pathlib.Path, help="retain per-thread sync/rename traces; timings include tracing overhead")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args(argv)
    if args.repetitions < 1 or (args.no_build and args.probe_binary is None):
        parser.error("positive repetitions and a binary for --no-build are required")
    # With one hold these remove 64 and 65 objects, crossing the inline
    # deletion batch boundary. Keep both cases in benchmark all's smoke run.
    counts = args.counts or ([65, 66, 128] if args.profile == "smoke" else [65, 66, 128, 1024])
    if min(counts) < 2:
        parser.error("counts must include at least one held and one garbage object")
    binary = args.probe_binary
    if binary is None:
        built = subprocess.run(["cargo", *CARGO_ARGUMENTS], cwd=cli.ROOT, capture_output=True, text=True)
        if built.returncode:
            raise common.BenchmarkError(built.stderr or built.stdout)
        binary = parse_probe_binary(built.stdout)
    binary = binary.resolve()
    if args.strace_dir:
        args.strace_dir = args.strace_dir.resolve()
        args.strace_dir.mkdir(parents=True, exist_ok=True)
    variants = [("candidate", binary)]
    if args.baseline_binary:
        variants.insert(0, ("baseline", args.baseline_binary.resolve()))
    artifacts = []
    for variant, executable in variants:
        with executable.open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        artifacts.append(dict(variant=variant, path=str(executable), sha256=digest))
    result = dict(schema_version=1, suite_id="collection-and-fsck", complete=False,
                  environment=common.environment_metadata(cli.ROOT),
                  fixture_filesystem=common.command_version(["findmnt", "--noheadings", "--output",
                      "FSTYPE,SOURCE,OPTIONS", "--target", tempfile.gettempdir()]),
                  configuration=dict(counts=counts, holds=args.holds,
                                     repetitions=args.repetitions, profile=args.profile,
                                     paired=bool(args.baseline_binary), traced=bool(args.strace_dir)),
                  artifacts=artifacts,
                  source_sha256={name: hashlib.sha256((cli.ROOT / name).read_bytes()).hexdigest()
                                 for name in ("crates/casita/src/blob/pack.rs", "crates/casita/src/blob/pack/shard.rs",
                                              "crates/casita/src/blob/pack/gc_benchmark.rs", "crates/casita/src/metadata/sqlite.rs",
                                              "crates/casita/src/sqlite.rs", "crates/casita/src/repository/publication.rs",
                                              "crates/casita/src/repository/collection_timing.rs",
                                              "crates/casita/benches/bench_util/gc_timing.rs", "benchmarks/suites/held_catalog_gc.py")},
                  samples=[], processes=[])

    def save():
        common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")

    save()
    try:
        # Interleave cases across repetitions; each sample uses a fresh process/cache.
        for repetition, count, holds in itertools.product(
                range(1, args.repetitions + 1), counts, args.holds):
            for variant, executable in (variants if repetition % 2 else variants[::-1]):
                command = [str(executable), PROBE, "--exact", "--ignored", "--nocapture"]
                trace_prefix = None
                if args.strace_dir:
                    trace_prefix = args.strace_dir / f"{variant}-{count}-{holds}-{repetition}"
                    if list(args.strace_dir.glob(trace_prefix.name + ".*")):
                        raise common.BenchmarkError("trace prefix already exists; use a fresh --strace-dir")
                    command = ["strace", "-ff", "-qq", "-ttt", "-T", "-yy",
                               "-e", "trace=fsync,fdatasync,rename,renameat,renameat2",
                               "-o", str(trace_prefix), *command]
                process = subprocess.run(command,
                    env={**os.environ, "CASITA_BENCH_GC_ENTRIES": str(count),
                         "CASITA_BENCH_GC_HOLDS": str(holds)},
                    capture_output=True, text=True)
                result["processes"].append(dict(variant=variant, count=count, holds=holds,
                    repetition=repetition, command=command, trace_prefix=str(trace_prefix) if trace_prefix else None, exit_code=process.returncode,
                    stdout=process.stdout, stderr=process.stderr))
                if process.returncode:
                    raise common.BenchmarkError("held-catalog-gc probe failed")
                sample = parse_sample(process.stdout, count, holds)
                if trace_prefix:
                    sample["sync_trace"] = sync_trace_summary(args.strace_dir.glob(trace_prefix.name + ".*"))
                sample["compaction"] = compaction_summary(sample["phases"])
                sample["release_compaction"] = compaction_summary(sample["release_phases"])
                result["samples"].append(dict(sample, variant=variant, status="ok", operation="held-catalog-gc",
                    entries=count, repetition=repetition, wall_seconds=sample["seconds"]))
                save()
        result["complete"] = True
    except Exception as error:
        result["error"] = str(error)
        raise
    finally:
        save()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
