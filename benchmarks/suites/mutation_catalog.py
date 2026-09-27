"""Mutation catalog retention across the former 64 MiB history limit."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import subprocess

from benchmarks import cli
from benchmarks.suites import repository as common
from benchmarks.suites.metadata_collection import CARGO_ARGUMENTS, positive_csv
from benchmarks.suites.pack.catalog import parse_probe_binary

PROBE = "repository::mutation_catalog_tests::benchmark_mutation_catalog_history"
CORRECTNESS = "bounded active catalog and empty released inventory"


def parse_sample(stdout, count):
    samples = [json.loads(line.removeprefix("catalog_history_sample "))
               for line in stdout.splitlines() if line.startswith("catalog_history_sample ")]
    if len(samples) != 1 or "test result: ok. 1 passed; 0 failed;" not in stdout:
        raise common.BenchmarkError("expected one passing catalog-history probe")
    sample = samples[0]
    if (sample.get("count") != count or sample.get("correctness") != CORRECTNESS
            or type(sample.get("nanos")) is not int or sample["nanos"] <= 0
            or sample.get("peak_catalog_bytes") != 1024 * 1024):
        raise common.BenchmarkError("invalid catalog-history configuration or correctness gate")
    return sample


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--counts", type=positive_csv, default=[63, 65])
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--probe-binary", type=pathlib.Path)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path)
    args = parser.parse_args(argv)
    if args.repetitions < 1 or (args.no_build and args.probe_binary is None):
        parser.error("positive repetitions and a binary for --no-build are required")
    binary = args.probe_binary
    if binary is None:
        built = subprocess.run(["cargo", *CARGO_ARGUMENTS], cwd=cli.ROOT, capture_output=True, text=True)
        if built.returncode:
            raise common.BenchmarkError(built.stderr or built.stdout)
        binary = parse_probe_binary(built.stdout)
    binary = binary.resolve()
    with binary.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    result = dict(schema_version=1, suite_id="state-and-publication", complete=False,
                  environment=common.environment_metadata(cli.ROOT),
                  configuration=dict(counts=args.counts, repetitions=args.repetitions,
                                     catalog_bytes=1024 * 1024, profile=args.profile),
                  artifacts=[dict(path=str(binary), sha256=digest)], samples=[], processes=[])

    def save():
        common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
        if args.report:
            lines = ["# Mutation catalog retention", "",
                     "Opaque 1 MiB catalog witnesses with a real file ledger; includes metadata updates, pin inspection, and release drain.",
                     "63 and 65 versions straddle the former cumulative 64 MiB limit. This is a controlled retention probe, not application throughput.",
                     "", f"Complete: {result['complete']}", "",
                     "| Versions | Repetition | Seconds | Peak catalog bytes |",
                     "|---:|---:|---:|---:|"]
            for sample in result["samples"]:
                lines.append(f"| {sample['entries']} | {sample['repetition']} | {sample['wall_seconds']:.6f} | {sample['peak_catalog_bytes']} |")
            if "error" in result:
                lines += ["", result["error"]]
            common.write_atomic(args.report, "\n".join(lines) + "\n")

    save()
    try:
        for count in args.counts:
            for repetition in range(1, args.repetitions + 1):
                process = subprocess.run(
                    [str(binary), PROBE, "--exact", "--ignored", "--nocapture"],
                    env={**os.environ, "CASITA_BENCH_CATALOG_VERSIONS": str(count)},
                    capture_output=True, text=True)
                result["processes"].append(dict(count=count, repetition=repetition,
                    exit_code=process.returncode, stdout=process.stdout, stderr=process.stderr))
                if process.returncode:
                    raise common.BenchmarkError("catalog-history probe failed")
                sample = parse_sample(process.stdout, count)
                result["samples"].append(dict(status="ok", operation="mutation-catalog-history",
                    entries=count, repetition=repetition, wall_seconds=sample["nanos"] / 1e9,
                    peak_catalog_bytes=sample["peak_catalog_bytes"], correctness=CORRECTNESS))
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
