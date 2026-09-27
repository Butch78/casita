"""Physical cleanup boundaries and large retirement queue memory."""
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

PROBE = "blob::pack::tests::benchmark_cleanup_batch_boundary"
QUEUE_PROBE = "blob::pack::tests::benchmark_retirement_queue_memory"
QUEUE_CORRECTNESS = "every candidate visited once; queue empty"
CORRECTNESS = "held path survives; candidates and claims reclaimed"


def parse_sample(stdout, count):
    samples = [json.loads(line.removeprefix("cleanup_batch_sample "))
               for line in stdout.splitlines() if line.startswith("cleanup_batch_sample ")]
    if len(samples) != 1 or "test result: ok. 1 passed; 0 failed;" not in stdout:
        raise common.BenchmarkError("expected one passing cleanup-batch probe")
    sample = samples[0]
    if (sample.get("count") != count or sample.get("correctness") != CORRECTNESS
            or type(sample.get("nanos")) is not int or sample["nanos"] <= 0
            or sample.get("claim_pairs") != (count + 999) // 1000):
        raise common.BenchmarkError("invalid cleanup-batch configuration or correctness gate")
    return sample


def parse_queue_sample(stdout, count):
    samples = [json.loads(line.removeprefix("retirement_queue_sample "))
               for line in stdout.splitlines() if line.startswith("retirement_queue_sample ")]
    if len(samples) != 1 or "test result: ok. 1 passed; 0 failed;" not in stdout:
        raise common.BenchmarkError("expected one passing retirement-queue probe")
    sample = samples[0]
    if (sample.get("count") != count or sample.get("correctness") != QUEUE_CORRECTNESS
            or type(sample.get("nanos")) is not int or sample["nanos"] <= 0
            or any(value is not None and (type(value) is not int or value < 0)
                   for value in (sample.get("rss_before"), sample.get("rss_at_first_delete")))):
        raise common.BenchmarkError("invalid retirement-queue sample")
    return sample


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--counts", type=positive_csv, default=[999, 1001])
    parser.add_argument("--queue-counts", type=positive_csv, default=[999, 1001, 100000])
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
    result = dict(schema_version=1, suite_id="blob-backends", complete=False,
                  environment=common.environment_metadata(cli.ROOT),
                  configuration=dict(counts=args.counts, queue_counts=args.queue_counts, repetitions=args.repetitions,
                                     batch_limit=1000, profile=args.profile),
                  artifacts=[dict(path=str(binary), sha256=digest)], samples=[], processes=[])

    def save():
        common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
        if args.report:
            lines = ["# Physical cleanup batches", "",
                     "Local filesystem deletion with a real file ledger and a disjoint held file. Setup and correctness checks are outside the timer.",
                     "999 and 1,001 garbage paths straddle the 1,000-path batch boundary. Each sample covers published retirement cleanup; orphan batching is additionally covered by tests.",
                     "", f"Complete: {result['complete']}", "",
                     "| Paths | Repetition | Seconds | Claim pairs |",
                     "|---:|---:|---:|---:|"]
            for sample in result["samples"]:
                if sample["operation"] != "cleanup-batches":
                    continue
                lines.append(f"| {sample['entries']} | {sample['repetition']} | {sample['wall_seconds']:.6f} | {sample['claim_pairs']} |")
            lines += ["", "Queue-only memory probe (RSS bytes; no payload files or durable ledger):", "",
                      "| Paths | Repetition | Seconds | RSS before | RSS at first delete |",
                      "|---:|---:|---:|---:|---:|"]
            for sample in result["samples"]:
                if sample["operation"] == "retirement-queue":
                    lines.append(f"| {sample['entries']} | {sample['repetition']} | {sample['wall_seconds']:.6f} | {sample['rss_before']} | {sample['rss_at_first_delete']} |")
            if "error" in result:
                lines += ["", result["error"]]
            common.write_atomic(args.report, "\n".join(lines) + "\n")

    save()
    try:
        for count in args.counts:
            for repetition in range(1, args.repetitions + 1):
                process = subprocess.run(
                    [str(binary), PROBE, "--exact", "--ignored", "--nocapture"],
                    env={**os.environ, "CASITA_BENCH_CLEANUP_PATHS": str(count)},
                    capture_output=True, text=True)
                result["processes"].append(dict(count=count, repetition=repetition,
                    exit_code=process.returncode, stdout=process.stdout, stderr=process.stderr))
                if process.returncode:
                    raise common.BenchmarkError("cleanup-batch probe failed")
                sample = parse_sample(process.stdout, count)
                result["samples"].append(dict(status="ok", operation="cleanup-batches",
                    entries=count, repetition=repetition, wall_seconds=sample["nanos"] / 1e9,
                    claim_pairs=sample["claim_pairs"], correctness=CORRECTNESS))
                save()
        for count in args.queue_counts:
            for repetition in range(1, args.repetitions + 1):
                process = subprocess.run(
                    [str(binary), QUEUE_PROBE, "--exact", "--ignored", "--nocapture"],
                    env={**os.environ, "CASITA_BENCH_CLEANUP_PATHS": str(count)},
                    capture_output=True, text=True)
                result["processes"].append(dict(operation="retirement-queue", count=count,
                    repetition=repetition, exit_code=process.returncode,
                    stdout=process.stdout, stderr=process.stderr))
                if process.returncode:
                    raise common.BenchmarkError("retirement-queue probe failed")
                sample = parse_queue_sample(process.stdout, count)
                result["samples"].append(dict(status="ok", operation="retirement-queue",
                    entries=count, repetition=repetition, wall_seconds=sample["nanos"] / 1e9,
                    rss_before=sample["rss_before"], rss_at_first_delete=sample["rss_at_first_delete"],
                    correctness=QUEUE_CORRECTNESS))
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
