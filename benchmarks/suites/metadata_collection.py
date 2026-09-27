"""Metadata collection and ordered inventory scaling."""
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
from benchmarks.suites.pack.catalog import parse_probe_binary

PROBE = "metadata::benchmarks::benchmark_metadata_collection"
CORRECTNESS = "zero removals, exact reopened inventory and revision"
CARGO_ARGUMENTS = ("test", "--release", "--features", "cli", "--lib", "--no-run", "--message-format=json")
SCAN_PROBE = "metadata::sqlite::tests::benchmark_ordered_scan"
SCAN_CORRECTNESS = "exact ordered records and snapshot revision"


def positive_csv(value):
    try:
        counts = [int(part) for part in value.split(",")]
    except ValueError as error:
        raise argparse.ArgumentTypeError("expected comma-separated integers") from error
    if not counts or min(counts) < 1 or len(set(counts)) != len(counts):
        raise argparse.ArgumentTypeError("counts must be positive and unique")
    return counts


def build_parser():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--probe", choices=("collection", "ordered-scan"), default="collection")
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--counts", type=positive_csv)
    parser.add_argument("--iterations", type=int, help="warm operations per case (smoke: 1, standard: 3)")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--probe-binary", type=pathlib.Path)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path)
    return parser


def parse_sample(stdout, count, iterations, probe="collection"):
    prefix = "scan_sample " if probe == "ordered-scan" else "collection_sample "
    correctness = SCAN_CORRECTNESS if probe == "ordered-scan" else CORRECTNESS
    try:
        cases = [json.loads(line.removeprefix(prefix))
                 for line in stdout.splitlines() if line.startswith(prefix)]
    except json.JSONDecodeError as error:
        raise common.BenchmarkError("invalid collection JSON") from error
    if "test result: ok. 1 passed; 0 failed;" not in stdout or len(cases) != 1:
        raise common.BenchmarkError("collection probe must execute one passing test and emit exactly one case")
    case = cases[0]
    if not isinstance(case, dict) or case.get("count") != count or case.get("iterations") != iterations or case.get("correctness") != correctness:
        raise common.BenchmarkError("collection case has wrong configuration or missing correctness gate")
    samples = case.get("samples")
    if not isinstance(samples, list) or len(samples) != iterations + 1:
        raise common.BenchmarkError("missing or duplicate collection iterations")
    for iteration, sample in enumerate(samples):
        if (not isinstance(sample, dict) or type(sample.get("iteration")) is not int
                or sample["iteration"] != iteration or sample.get("warm") is not (iteration > 0)
                or type(sample.get("nanos")) is not int or sample["nanos"] < 0):
            raise common.BenchmarkError("invalid collection timing or iteration order")
    return case


def save(args, result):
    common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
    lines = ["# Metadata collection", "",
             "Leaf-only metadata; every object retained. Setup and reopened inventory audit are outside commit timing.",
             "First means the first collection after seeding, not a cold OS cache. Warm values are means per process; raw iterations are retained.",
             "Process RSS includes setup and audit. These are local measurements, not a controlled revision comparison.",
             "", f"Complete: {result['complete']}", ""]
    if args.probe == "ordered-scan":
        lines = ["# Ordered metadata scan", "",
                 "Verified blobs in a reopened snapshot; exact records and canonical order checked after every scan.",
                 "Timing includes consuming and decoding the full stream. Seeding, reopen and correctness comparisons are outside timing.",
                 "First means the first scan of the snapshot, not a cold OS cache. Warm values are means per process; raw scans are retained.",
                 "Process RSS includes setup and audits. These are local measurements, not a controlled revision comparison.",
                 "", f"Complete: {result['complete']}", ""]
    if "error" in result:
        lines += [f"Error: {result['error']}", ""]
    lines += ["| Objects | Phase | Repetition | Mean operation ms |", "|---:|---|---:|---:|"]
    for sample in result["samples"]:
        lines.append(f"| {sample['entries']} | {sample['operation']} | {sample['repetition']} | {sample['wall_seconds'] * 1000:.3f} |")
    common.write_atomic(args.report or args.output.with_suffix(".md"), "\n".join(lines) + "\n")


def main(argv=None):
    parser = build_parser()
    args = parser.parse_args(argv)
    counts = args.counts or ([65536, 65537] if args.profile == "smoke" else [256, 8192, 65536, 65537])
    scan = args.probe == "ordered-scan"
    if scan and args.counts is None:
        counts = [256, 257, 8192] if args.profile == "smoke" else [256, 257, 8192, 65536, 65537]
    iterations = args.iterations if args.iterations is not None else (1 if args.profile == "smoke" else 3)
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
    with tempfile.TemporaryDirectory(prefix="casita-metadata-collection-") as temporary:
        work = pathlib.Path(temporary)
        result = {"schema_version": 1, "result_schema": "casita.metadata-scan.v1" if scan else "casita.metadata-collection.v1",
                  "suite_id": "state-and-publication" if scan else "collection-and-fsck", "complete": False,
                  "environment": common.environment_metadata(work),
                  "configuration": {"profile": args.profile, "counts": counts, "iterations": iterations,
                                    "repetitions": args.repetitions, "fixture": "ordered-blobs" if scan else "all-retained-blobs", "seed_batch_size": 1024},
                  "artifacts": [{"path": str(binary), "sha256": digest}], "samples": [], "processes": []}
        save(args, result)
        schedule = [(rep, count) for rep in range(1, args.repetitions + 1) for count in counts]
        random.Random(0xCA517A).shuffle(schedule)
        try:
            for repetition, count in schedule:
                print(f"metadata-{args.probe}: {count} objects, repetition {repetition}", flush=True)
                env = {**os.environ, "CASITA_COLLECTION_COUNTS": str(count), "CASITA_COLLECTION_ITERATIONS": str(iterations)}
                stdout, stderr = work / "stdout", work / "stderr"
                timing = common.measured_command(common.CommandSpec(
                    [[str(binary), SCAN_PROBE if scan else PROBE, "--exact", "--ignored", "--nocapture"]], work, env), stdout, stderr, check=False)
                captured = stdout.read_text()
                result["processes"].append({**timing, "entries": count, "repetition": repetition,
                                            "stdout": captured, "stderr": stderr.read_text()})
                if timing["exit_code"] != 0:
                    raise common.BenchmarkError(f"{args.probe} probe failed ({timing['exit_code']}): {stderr.read_text()}")
                case = parse_sample(captured, count, iterations, args.probe)
                for phase, commits in (("first", case["samples"][:1]), ("warm", case["samples"][1:])):
                    result["samples"].append({"status": "ok", "implementation": "casita",
                        "operation": f"{'scan' if scan else 'collect'}-{phase}", "entries": count, "repetition": repetition,
                        "wall_seconds": statistics.mean(sample["nanos"] for sample in commits) / 1e9,
                        "max_rss_bytes": timing["max_rss_bytes"], "scans" if scan else "commits": commits, "correctness": case["correctness"]})
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
