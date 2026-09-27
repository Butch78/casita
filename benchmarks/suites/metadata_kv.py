"""Mutable metadata get, prefix scan, and checked commits on local Turso."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import random
import statistics
import subprocess
import tempfile

from benchmarks import cli
from benchmarks.suites import repository as common
from benchmarks.suites.metadata_collection import CARGO_ARGUMENTS, positive_csv
from benchmarks.suites.pack.catalog import parse_probe_binary

PROBE = "metadata::kv_benchmarks::benchmark_metadata_kv"
CORRECTNESS = "exact values and prefix pages, atomic checks and roots, concurrent winners, persisted indexes independent of GC"
OPERATIONS = ("get-hit", "get-miss", "get-batch", "get-batch-scalar", "scan-first", "scan-deep", "scan-prefix",
              "commit-insert", "commit-replace", "commit-delete", "commit-conflict-first", "commit-conflict-last",
              "commit-register-root", "commit-disjoint", "commit-contended")

OPERATIONS += tuple(f"get-current-{reference}{width}" for width in (1, 7, 8, 9, 15, 16, 17) for reference in ("", "reference-"))

def build_parser():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--counts", type=positive_csv)
    parser.add_argument("--batches", type=positive_csv)
    parser.add_argument("--iterations", type=int)
    parser.add_argument("--value-bytes", type=int, default=256)
    parser.add_argument("--page-size", type=int, default=16)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--probe-binary", type=pathlib.Path)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path)
    return parser


def parse_sample(stdout, count, batch, iterations, value_bytes=256, page_size=16):
    try:
        cases = [json.loads(line.removeprefix("primitive_sample "))
                 for line in stdout.splitlines() if line.startswith("primitive_sample ")]
    except json.JSONDecodeError as error:
        raise common.BenchmarkError("invalid primitive JSON") from error
    if "test result: ok. 1 passed; 0 failed;" not in stdout or len(cases) != 1:
        raise common.BenchmarkError("primitive probe must execute one passing test and emit one case")
    case = cases[0]
    if (not isinstance(case, dict) or case.get("count") != count or case.get("batch") != batch
            or case.get("iterations") != iterations or case.get("correctness") != CORRECTNESS):
        raise common.BenchmarkError("wrong primitive configuration or missing correctness gate")
    if case.get("value_bytes") != value_bytes or case.get("page_size") != page_size or case.get("fanout") != 257:
        raise common.BenchmarkError("wrong metadata value size, page size or fanout")
    samples = case.get("samples")
    if not isinstance(samples, list):
        raise common.BenchmarkError("missing primitive samples")
    expected = {(op, iteration) for op in OPERATIONS for iteration in range(iterations)}
    seen = set()
    for sample in samples:
        if (not isinstance(sample, dict) or not isinstance(sample.get("operation"), str)
                or type(sample.get("iteration")) is not int or type(sample.get("nanos")) is not int
                or sample["nanos"] < 0):
            raise common.BenchmarkError("invalid primitive timing")
        identity = (sample["operation"], sample["iteration"])
        if identity not in expected or identity in seen:
            raise common.BenchmarkError("unexpected or duplicate primitive sample")
        seen.add(identity)
    if seen != expected:
        raise common.BenchmarkError("incomplete primitive operations")
    return case


def save(args, result):
    common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
    lines = ["# Local mutable metadata primitives", "",
             "Public get/scan/commit APIs on Turso. Prefix scans use 257 matching reverse references among a growing unrelated inventory.",
             "Scalar and batch reads use identical keys and a held snapshot; setup, verification and correctness audits are excluded.",
             "Current reads compare short transactions with snapshot acquisition at widths 1/7/8/9/15/16/17 around the current eight-idle limit and historical sixteen-idle boundary; paired order alternates. Both paths share the same idle connection pool and repository-state validation.",
             "Process RSS includes fixture construction and audits. Reads follow reopen without flushing OS caches.",
             "Registration adds a descriptor, batch reverse entries and a root atomically. Contention timings include task scheduling for two writers.",
             "", f"Complete: {result['complete']}", ""]
    if "error" in result:
        lines += [f"Error: {result['error']}", ""]
    lines += ["| Initial records | Batch | Operation | Repetition | Median ms |", "|---:|---:|---|---:|---:|"]
    for sample in result["samples"]:
        lines.append(f"| {sample['entries']} | {sample['batch']} | {sample['operation']} | {sample['repetition']} | {sample['wall_seconds'] * 1000:.3f} |")
    common.write_atomic(args.report or args.output.with_suffix(".md"), "\n".join(lines) + "\n")


def main(argv=None):
    parser = build_parser()
    args = parser.parse_args(argv)
    smoke = args.profile == "smoke"
    counts = args.counts or ([256, 257, 8191, 8192] if smoke else [256, 257, 8191, 8192, 65536, 65537])
    batches = args.batches or ([1, 16] if smoke else [1, 16, 256, 257])
    iterations = args.iterations if args.iterations is not None else (2 if smoke else 10)
    if not 1 <= args.value_bytes <= 4096 or not 1 <= args.page_size <= 1024 or max(batches) > 1024:
        parser.error("value-bytes must be 1..4096, page-size and batches 1..1024")
    if iterations < 1 or args.repetitions < 1:
        parser.error("iterations and repetitions must be positive")
    if args.no_build and args.probe_binary is None:
        parser.error("--no-build requires --probe-binary")
    binary = args.probe_binary
    if binary is None:
        built = subprocess.run(["cargo", *CARGO_ARGUMENTS], cwd=cli.ROOT, capture_output=True, text=True)
        if built.returncode:
            raise common.BenchmarkError(built.stderr or built.stdout)
        binary = parse_probe_binary(built.stdout)
    binary = binary.resolve()
    with binary.open("rb") as handle:
        digest = hashlib.file_digest(handle, "sha256").hexdigest()
    with tempfile.TemporaryDirectory(prefix="casita-metadata-kv-") as temporary:
        work = pathlib.Path(temporary)
        result = {"schema_version": 1, "result_schema": "casita.metadata-kv.v1",
                  "suite_id": "state-and-publication", "complete": False,
                  "environment": common.environment_metadata(work),
                  "configuration": {"profile": args.profile, "counts": counts, "batches": batches,
                                    "iterations": iterations, "repetitions": args.repetitions,
                                    "contract": "mutable-metadata", "value_bytes": args.value_bytes, "page_size": args.page_size, "fanout": 257, "backend": "turso"},
                  "artifacts": [{"path": str(binary), "sha256": digest}], "samples": [], "processes": []}
        save(args, result)
        schedule = [(rep, count, batch) for rep in range(1, args.repetitions + 1)
                    for count in counts for batch in batches]
        random.Random(0xCA517A).shuffle(schedule)
        try:
            for repetition, count, batch in schedule:
                print(f"metadata-kv: {count} records, batch {batch}, repetition {repetition}", flush=True)
                env = {**os.environ, "CASITA_PRIMITIVE_ENTRIES": str(count), "CASITA_PRIMITIVE_BATCH": str(batch),
                       "CASITA_PRIMITIVE_ITERATIONS": str(iterations), "CASITA_KV_VALUE_BYTES": str(args.value_bytes), "CASITA_KV_PAGE_SIZE": str(args.page_size)}
                stdout, stderr = work / "stdout", work / "stderr"
                timing = common.measured_command(common.CommandSpec(
                    [[str(binary), PROBE, "--exact", "--ignored", "--nocapture"]], work, env), stdout, stderr, check=False)
                captured = stdout.read_text()
                result["processes"].append({**timing, "entries": count, "batch": batch, "repetition": repetition,
                                            "stdout": captured, "stderr": stderr.read_text()})
                if timing["exit_code"] != 0:
                    raise common.BenchmarkError(f"primitive probe failed: {stderr.read_text()}")
                case = parse_sample(captured, count, batch, iterations, args.value_bytes, args.page_size)
                for operation in OPERATIONS:
                    observations = [s for s in case["samples"] if s["operation"] == operation]
                    result["samples"].append({"status": "ok", "implementation": "casita",
                        "operation": operation, "entries": count, "batch": batch, "value_bytes": args.value_bytes, "page_size": args.page_size,
                        "corpus": f"records-{count}-batch-{batch}", "repetition": repetition,
                        "wall_seconds": statistics.median(s["nanos"] for s in observations) / 1e9,
                        "metrics": {f"p{p}_nanos": sorted(s["nanos"] for s in observations)[min(len(observations) - 1, (len(observations) * p + 99) // 100 - 1)] for p in (50, 95, 99)},
                        "max_rss_bytes": timing["max_rss_bytes"], "timings": observations,
                        "correctness": case["correctness"]})
                save(args, result)
        except Exception as error:
            result["error"] = str(error)
            save(args, result)
            raise
        result["complete"] = True
        save(args, result)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
