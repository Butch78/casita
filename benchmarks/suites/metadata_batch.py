"""Direct metadata batch reads against the frozen per-key reference."""
from __future__ import annotations

import argparse
import hashlib
import itertools
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

PROBE = "metadata::sqlite::tests::benchmark_object_batch"
CORRECTNESS = "exact ordered records, duplicates, misses and snapshot isolation"
VARIANTS = ("point-reference", "current")
PATTERNS = ("hits", "mixed")
WIDTHS = [1, 127, 128, 129, 255, 256, 257, 1024]


def fingerprint(path):
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def build_parser():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--counts", type=positive_csv)
    parser.add_argument("--widths", type=positive_csv, default=WIDTHS)
    parser.add_argument("--requests", type=int)
    parser.add_argument("--iterations", type=int)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--probe-binary", type=pathlib.Path)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path)
    return parser


def parse_sample(stdout, count, widths, requests, iterations):
    try:
        cases = [json.loads(line.removeprefix("batch_sample "))
                 for line in stdout.splitlines() if line.startswith("batch_sample ")]
    except json.JSONDecodeError as error:
        raise common.BenchmarkError("invalid batch JSON") from error
    if "test result: ok. 1 passed; 0 failed;" not in stdout or len(cases) != 1:
        raise common.BenchmarkError("batch probe must run one passing test and emit one case")
    case = cases[0]
    if not isinstance(case, dict) or any(case.get(key) != value for key, value in {
        "count": count, "widths": widths, "requests": requests, "iterations": iterations,
        "correctness": CORRECTNESS,
    }.items()):
        raise common.BenchmarkError("wrong batch configuration or missing correctness gate")
    expected = set(itertools.product(PATTERNS, widths, range(iterations + 1), VARIANTS))
    seen = set()
    if not isinstance(case.get("samples"), list):
        raise common.BenchmarkError("missing batch samples")
    for sample in case["samples"]:
        if not isinstance(sample, dict):
            raise common.BenchmarkError("invalid batch sample")
        key = tuple(sample.get(field) for field in ("pattern", "width", "iteration", "variant"))
        if (type(sample.get("width")) is not int or type(sample.get("iteration")) is not int
                or type(sample.get("nanos")) is not int or sample["nanos"] < 0
                or sample.get("warm") is not (sample["iteration"] > 0)
                or key not in expected or key in seen):
            raise common.BenchmarkError("invalid or duplicate batch timing")
        seen.add(key)
    if seen != expected:
        raise common.BenchmarkError("incomplete batch matrix")
    return case


def save(args, result):
    common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
    lines = ["# Metadata batch reads", "",
             "Both implementations share a reopened read snapshot. The point reference reproduces a0a8b89's one-task, one-lock batch of cached single-key queries.",
             "Seed, reopen, exact output checks and snapshot isolation checks are outside timing. Timings include API dispatch, key cloning, decoding and collecting results.",
             "Paths alternate within each iteration. First is not a cold cache; the inventory audit precedes measurements. RSS includes setup and both paths.",
             "", f"Complete: {result['complete']}", "",
             "| Objects | Pattern | Width | Phase | Path | Repetition | Read ms |",
             "|---:|---|---:|---|---|---:|---:|"]
    for sample in result["samples"]:
        lines.append(f"| {sample['entries']} | {sample['pattern']} | {sample['batch_width']} | {sample['operation']} | {sample['variant']} | {sample['repetition']} | {sample['wall_seconds'] * 1000:.3f} |")
    if "error" in result:
        lines += ["", f"Error: {result['error']}"]
    common.write_atomic(args.report or args.output.with_suffix(".md"), "\n".join(lines) + "\n")


def main(argv=None):
    parser = build_parser()
    args = parser.parse_args(argv)
    smoke = args.profile == "smoke"
    counts = args.counts or ([512] if smoke else [8192, 65536])
    requests = args.requests if args.requests is not None else (1024 if smoke else 4096)
    iterations = args.iterations if args.iterations is not None else (1 if smoke else 3)
    if min(requests, iterations, args.repetitions) < 1:
        parser.error("requests, iterations and repetitions must be positive")
    if args.no_build and args.probe_binary is None:
        parser.error("--no-build requires --probe-binary")
    binary = args.probe_binary
    if binary is None:
        built = subprocess.run(["cargo", *CARGO_ARGUMENTS], cwd=cli.ROOT, capture_output=True, text=True)
        if built.returncode:
            raise common.BenchmarkError(built.stderr or built.stdout)
        binary = parse_probe_binary(built.stdout)
    binary = binary.resolve()
    digest = fingerprint(binary)
    with tempfile.TemporaryDirectory(prefix="casita-metadata-batch-") as temporary:
        work = pathlib.Path(temporary)
        result = {"schema_version": 1, "result_schema": "casita.metadata-batch.v1",
                  "suite_id": "state-and-publication", "complete": False,
                  "environment": common.environment_metadata(work),
                  "configuration": {"profile": args.profile, "counts": counts, "widths": args.widths,
                                    "requests": requests, "iterations": iterations, "repetitions": args.repetitions,
                                    "fixture": "three-namespace-structural-records", "reference_revision": "a0a8b8941feca5553478c32b2e03c5bd2bf68d7d"},
                  "artifacts": [{"path": str(binary), "sha256": digest}], "samples": [], "processes": []}
        save(args, result)
        schedule = list(itertools.product(range(1, args.repetitions + 1), counts))
        random.Random(0xCA517A).shuffle(schedule)
        try:
            for repetition, count in schedule:
                print(f"metadata-batch: {count} objects, repetition {repetition}", flush=True)
                env = {**os.environ, "CASITA_BATCH_COUNT": str(count), "CASITA_BATCH_WIDTHS": ",".join(map(str, args.widths)),
                       "CASITA_BATCH_REQUESTS": str(requests), "CASITA_BATCH_ITERATIONS": str(iterations)}
                stdout, stderr = work / "stdout", work / "stderr"
                timing = common.measured_command(common.CommandSpec(
                    [[str(binary), PROBE, "--exact", "--ignored", "--nocapture"]], work, env), stdout, stderr, check=False)
                captured = stdout.read_text()
                result["processes"].append({**timing, "entries": count, "repetition": repetition,
                                            "stdout": captured, "stderr": stderr.read_text()})
                if timing["exit_code"] != 0:
                    raise common.BenchmarkError(f"batch probe failed ({timing['exit_code']}): {stderr.read_text()}")
                case = parse_sample(captured, count, args.widths, requests, iterations)
                for pattern, width, variant, phase in itertools.product(PATTERNS, args.widths, VARIANTS, ("first", "warm")):
                    samples = [sample for sample in case["samples"] if sample["pattern"] == pattern
                               and sample["width"] == width and sample["variant"] == variant
                               and sample["warm"] == (phase == "warm")]
                    result["samples"].append({"status": "ok", "operation": f"batch-{phase}",
                        "entries": count, "pattern": pattern, "batch_width": width, "variant": variant,
                        "repetition": repetition, "requests": requests,
                        "wall_seconds": statistics.mean(sample["nanos"] for sample in samples) / 1e9,
                        "max_rss_bytes": timing["max_rss_bytes"], "reads": samples, "correctness": CORRECTNESS})
                save(args, result)
            if fingerprint(binary) != digest:
                raise common.BenchmarkError("probe binary changed during measurements")
        except Exception as error:
            result["error"] = str(error)
            save(args, result)
            raise
        result["complete"] = True
        save(args, result)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
