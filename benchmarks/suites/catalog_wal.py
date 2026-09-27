"""SQLite WAL footprint of production catalog encodings and a small-reference control."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import subprocess

from benchmarks import cli
from benchmarks.suites import repository as common
from benchmarks.suites.metadata_collection import CARGO_ARGUMENTS
from benchmarks.suites.pack.catalog import parse_probe_binary

PROBE = "blob::pack::benchmarks::benchmark_catalog_wal"
CASES = ("small", "delta-below", "delta-above", "base-below", "base-above")
MODES = ("resubmit", "metadata-only", "reference", "external")
CORRECTNESS = "catalog boundary and membership, retained snapshot, exact reopened catalog and generation, released WAL truncated"


def parse_sample(stdout, case, mode, held, iterations):
    samples = [json.loads(line.removeprefix("catalog_wal_sample "))
               for line in stdout.splitlines() if line.startswith("catalog_wal_sample ")]
    if len(samples) != 1 or "test result: ok. 1 passed; 0 failed;" not in stdout:
        raise common.BenchmarkError("expected one passing catalog WAL probe")
    sample = samples[0]
    expected = dict(case=case, mode=mode, held=held, iterations=iterations,
                    correctness=CORRECTNESS, after_release_bytes=0)
    if any(sample.get(key) != value for key, value in expected.items()):
        raise common.BenchmarkError("catalog WAL configuration or correctness mismatch")
    for key in ("catalog_bytes", "stored_bytes", "nanos", "before_checkpoint_bytes", "database_bytes"):
        if type(sample.get(key)) is not int or sample[key] <= 0:
            raise common.BenchmarkError(f"invalid {key}")
    if (len(sample.get("wal_sizes", [])) != iterations
            or any(type(size) is not int or size < 0 for size in sample["wal_sizes"])
            or len(sample.get("checkpoint", [])) != 3):
        raise common.BenchmarkError("invalid WAL observations")
    if mode == "reference" and sample["stored_bytes"] != 48:
        raise common.BenchmarkError("reference control must be 48 bytes")
    if mode == "external" and sample["stored_bytes"] != 56:
        raise common.BenchmarkError("external root witness must be 56 bytes")
    busy, log, done = sample["checkpoint"]
    if type(busy) is not int or busy not in (0, 1):
        raise common.BenchmarkError("invalid checkpoint status")
    if any(not (type(value) is int and value >= 0) and not (busy == 1 and value is None)
           for value in (log, done)):
        raise common.BenchmarkError("invalid checkpoint frame counts")
    return sample


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--iterations", type=int)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--probe-binary", type=pathlib.Path)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path)
    args = parser.parse_args(argv)
    iterations = args.iterations or (8 if args.profile == "smoke" else 32)
    if min(iterations, args.repetitions) < 1 or args.iterations == 0:
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
    with binary.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    result = dict(schema_version=1, suite_id="state-and-publication", complete=False,
                  environment=common.environment_metadata(cli.ROOT),
                  configuration=dict(iterations=iterations, repetitions=args.repetitions,
                                     profile=args.profile, cases=CASES, modes=MODES),
                  artifacts=[dict(path=str(binary), sha256=digest)], samples=[], processes=[])

    def save():
        common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
        lines = ["# Catalog WAL footprint", "", f"Complete: {result['complete']}", "",
                 "Real catalog encodings and Turso metadata commits; payload objects are not materialized.",
                 "Reference mode models SQL storage only; external mode includes durable root publication. Timing excludes fixture construction and audits.",
                 "WAL bytes are file length, not cumulative write traffic. PASSIVE checkpoint runs after measurement.",
                 "", "| Case | Mode | Held reader | Rep | Catalog bytes | SQL bytes | WAL bytes | Checkpoint (busy/log/done) |",
                 "|---|---|---|---:|---:|---:|---:|---|"]
        for s in result["samples"]:
            lines.append(f"| {s['case']} | {s['mode']} | {s['held']} | {s['repetition']} | {s['catalog_bytes']} | {s['stored_bytes']} | {s['before_checkpoint_bytes']} | {s['checkpoint']} |")
        if "error" in result:
            lines += ["", result["error"]]
        common.write_atomic(args.report or args.output.with_suffix(".md"), "\n".join(lines) + "\n")

    save()
    try:
        for repetition in range(1, args.repetitions + 1):
            for case in CASES:
                for mode in MODES:
                    for held in (False, True):
                        print(f"catalog-wal: {case}/{mode}/held={held} rep={repetition}", flush=True)
                        env = {**os.environ, "CASITA_WAL_CASE": case, "CASITA_WAL_MODE": mode,
                               "CASITA_WAL_HELD": str(held).lower(), "CASITA_WAL_ITERATIONS": str(iterations)}
                        process = subprocess.run([str(binary), PROBE, "--exact", "--ignored", "--nocapture"],
                                                 env=env, capture_output=True, text=True, timeout=180)
                        result["processes"].append(dict(case=case, mode=mode, held=held, repetition=repetition,
                            exit_code=process.returncode, stdout=process.stdout, stderr=process.stderr))
                        if process.returncode:
                            raise common.BenchmarkError(f"catalog WAL probe failed: {process.stderr}")
                        sample = parse_sample(process.stdout, case, mode, held, iterations)
                        result["samples"].append(dict(sample, status="ok", operation="catalog-wal",
                            variant=f"{case}/{mode}/held={held}", entries=sample["catalog_bytes"],
                            repetition=repetition, wall_seconds=sample["nanos"] / 1e9 / iterations))
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
