"""Durable small-blob staging across the FastCDC minimum-size boundary."""
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

PROBE = "blob::chunked::tests::benchmark_small_blob_pins"
CORRECTNESS = "exact readback, duplicate identity, no duplicate edits, no leaked pins"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--counts", type=positive_csv)
    parser.add_argument("--sizes", type=positive_csv, default=[64, 511, 512, 513, 2047, 2048, 2049])
    parser.add_argument("--layouts", nargs="+", choices=("packed", "loose"), default=["packed", "loose"])
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--probe-binary", type=pathlib.Path)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path)
    args = parser.parse_args(argv)
    counts = args.counts or ([16] if args.profile == "smoke" else [64, 256])
    if args.repetitions < 1 or min(args.sizes) < 8 or (args.no_build and args.probe_binary is None):
        parser.error("positive repetitions, sizes >= 8 and a binary for --no-build are required")
    binary = args.probe_binary
    if binary is None:
        build = subprocess.run(["cargo", *CARGO_ARGUMENTS], cwd=cli.ROOT, capture_output=True, text=True)
        if build.returncode:
            raise common.BenchmarkError(build.stderr or build.stdout)
        binary = parse_probe_binary(build.stdout)
    binary = binary.resolve()
    with binary.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    result = dict(schema_version=1, suite_id="state-and-publication", complete=False,
                  environment=common.environment_metadata(cli.ROOT),
                  configuration=dict(counts=counts, sizes=args.sizes, repetitions=args.repetitions,
                                     average_chunk_bytes=1024, profile=args.profile, layouts=args.layouts),
                  artifacts=[dict(path=str(binary), sha256=digest)], samples=[], processes=[])

    def save():
        common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
        if args.report:
            lines = ["# Small-blob pin protection", "", f"Complete: {result['complete']}", "",
                     "Fresh local payload store (loose and packed) and durable pin ledger. Timings cover staging only; publication, readback, duplicate writes and release checks are outside timing.",
                     "Ledger edits are revision advances, not syscall counts. Raw process output and binary identity are retained in JSON.", "",
                     "| Layout | Files | Bytes/file | Repetition | Seconds | Ledger edits |", "|---|---:|---:|---:|---:|---:|"]
            for sample in result["samples"]:
                lines.append(f"| {sample['layout']} | {sample['entries']} | {sample['bytes_per_file']} | {sample['repetition']} | {sample['wall_seconds']:.6f} | {sample['ledger_edits']} |")
            if "error" in result:
                lines += ["", result["error"]]
            common.write_atomic(args.report, "\n".join(lines) + "\n")

    save()
    try:
        for count in counts:
            for layout, size in itertools.product(args.layouts, args.sizes):
                for repetition in range(1, args.repetitions + 1):
                    process = subprocess.run([str(binary), PROBE, "--exact", "--ignored", "--nocapture"],
                        env={**os.environ, "CASITA_BENCH_BLOBS": str(count), "CASITA_BENCH_BLOB_BYTES": str(size), "CASITA_BENCH_PIN_LAYOUT": layout},
                        capture_output=True, text=True)
                    result["processes"].append(dict(layout=layout, count=count, size=size, repetition=repetition,
                        exit_code=process.returncode, stdout=process.stdout, stderr=process.stderr))
                    rows = [json.loads(line.removeprefix("small_blob_pins_sample "))
                            for line in process.stdout.splitlines() if line.startswith("small_blob_pins_sample ")]
                    if process.returncode or len(rows) != 1 or "test result: ok. 1 passed; 0 failed;" not in process.stdout:
                        raise common.BenchmarkError("expected exactly one passing small-blob probe")
                    sample = rows[0]
                    if (sample.get("layout") != layout or sample.get("count") != count or sample.get("bytes") != size
                            or sample.get("correctness") != CORRECTNESS
                            or type(sample.get("nanos")) is not int or sample["nanos"] <= 0
                            or type(sample.get("ledger_edits")) is not int or sample["ledger_edits"] < count):
                        raise common.BenchmarkError("invalid small-blob sample or correctness gate")
                    result["samples"].append(dict(status="ok", operation="small-blob-staging", layout=layout, entries=count,
                        bytes_per_file=size, repetition=repetition, wall_seconds=sample["nanos"] / 1e9,
                        ledger_edits=sample["ledger_edits"], correctness=CORRECTNESS))
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
