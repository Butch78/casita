"""Historical catalog marking with identical, overlapping and disjoint holds."""
from __future__ import annotations

import argparse
import hashlib
import itertools
import json
import os
import pathlib
import subprocess

from benchmarks import cli
from benchmarks.suites import repository as common
from benchmarks.suites.metadata_collection import CARGO_ARGUMENTS, positive_csv
from benchmarks.suites.pack.catalog import parse_probe_binary

PROBE = "blob::pack::tests::benchmark_catalog_marking"
CORRECTNESS = "exact union of pack, chunk outboard, blob and blob outboard paths"
MODES = ("identical", "overlap", "disjoint")


def parse_sample(stdout, count, holds, mode):
    rows = [json.loads(line.removeprefix("catalog_marking_sample "))
            for line in stdout.splitlines() if line.startswith("catalog_marking_sample ")]
    if len(rows) != 1 or "test result: ok. 1 passed; 0 failed;" not in stdout:
        raise common.BenchmarkError("expected one passing catalog-marking probe")
    row = rows[0]
    if (row.get("count") != count or row.get("holds") != holds or row.get("mode") != mode
            or row.get("correctness") != CORRECTNESS
            or row.get("paths") != count * 4 * (holds if mode == "disjoint" else 1)
            or any(type(row.get(key)) is not int or row[key] <= 0
                   for key in ("nanos", "path_bytes", "index_requests", "index_bytes"))
            or any(key not in row or (row[key] is not None and
                   (type(row[key]) is not int or row[key] < 0))
                   for key in ("rss_before", "rss_after"))):
        raise common.BenchmarkError("invalid catalog-marking configuration or correctness gate")
    return row


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--counts", type=positive_csv)
    parser.add_argument("--holds", type=positive_csv, default=[1, 8])
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--probe-binary", type=pathlib.Path)
    parser.add_argument("--baseline-binary", type=pathlib.Path)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args(argv)
    if args.repetitions < 1 or (args.no_build and args.probe_binary is None):
        parser.error("positive repetitions and a binary for --no-build are required")
    counts = args.counts or ([1024] if args.profile == "smoke" else [1024, 65536])
    binary = args.probe_binary
    if binary is None:
        built = subprocess.run(["cargo", *CARGO_ARGUMENTS], cwd=cli.ROOT, capture_output=True, text=True)
        if built.returncode:
            raise common.BenchmarkError(built.stderr or built.stdout)
        binary = parse_probe_binary(built.stdout)
    binary = binary.resolve()
    variants = [("candidate", binary)]
    if args.baseline_binary:
        variants.insert(0, ("baseline", args.baseline_binary.resolve()))
    artifacts = []
    for variant, executable in variants:
        with executable.open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        artifacts.append(dict(variant=variant, path=str(executable), sha256=digest))
    result = dict(schema_version=1, suite_id="blob-backends", complete=False,
                  environment=common.environment_metadata(cli.ROOT),
                  configuration=dict(counts=counts, holds=args.holds, modes=MODES,
                                     repetitions=args.repetitions, profile=args.profile,
                                     paired=bool(args.baseline_binary)),
                  artifacts=artifacts,
                  source_sha256={name: hashlib.sha256((cli.ROOT / name).read_bytes()).hexdigest()
                                 for name in ("crates/casita/src/blob/pack.rs", "crates/casita/src/blob/pack/shard.rs",
                                              "benchmarks/suites/catalog_marking.py")},
                  samples=[], processes=[])

    def save():
        common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")

    save()
    try:
        # Interleave cases across repetitions; each sample uses a fresh process/cache.
        for repetition, count, holds, mode in itertools.product(
                range(1, args.repetitions + 1), counts, args.holds, MODES):
            if holds == 1 and mode != "identical":
                continue
            for variant, executable in (variants if repetition % 2 else variants[::-1]):
                process = subprocess.run([str(executable), PROBE, "--exact", "--ignored", "--nocapture"],
                    env={**os.environ, "CASITA_BENCH_MARK_ENTRIES": str(count),
                         "CASITA_BENCH_MARK_HOLDS": str(holds), "CASITA_BENCH_MARK_MODE": mode},
                    capture_output=True, text=True)
                result["processes"].append(dict(variant=variant, count=count, holds=holds, mode=mode,
                    repetition=repetition, exit_code=process.returncode,
                    stdout=process.stdout, stderr=process.stderr))
                if process.returncode:
                    raise common.BenchmarkError("catalog-marking probe failed")
                sample = parse_sample(process.stdout, count, holds, mode)
                result["samples"].append(dict(sample, variant=variant, status="ok", operation="catalog-marking",
                    entries=count, repetition=repetition, wall_seconds=sample["nanos"] / 1e9))
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
