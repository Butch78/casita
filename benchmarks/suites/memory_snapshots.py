"""Memory metadata snapshot and publication scaling with a retained revision."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import random
import subprocess
import tempfile

from benchmarks import cli
from benchmarks.suites import repository as common
from benchmarks.suites.metadata_collection import CARGO_ARGUMENTS, positive_csv
from benchmarks.suites.pack.catalog import parse_probe_binary

PROBE = "metadata::benchmarks::benchmark_memory_snapshots"
CORRECTNESS = "retained revision, object, root, validation and catalog unchanged; exact published root and inventory"


def save(args, result):
    common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
    lines = ["# Memory snapshots", "", f"Complete: {result['complete']}", "",
             "Snapshot timing includes acquisition and drop; publication holds an older revision.",
             "Deep-copy is an in-process control reproducing the previous snapshot algorithm; both variants use the current commit algorithm.",
             "Publication excludes snapshot acquisition. Seeding and correctness audits are excluded; RSS includes them.", "",
             "| Entries | Variant | Operation | Repetition | ms/op |", "|---:|---|---|---:|---:|"]
    for sample in result["samples"]:
        lines.append(f"| {sample['entries']} | {sample['variant']} | {sample['operation']} | {sample['repetition']} | {sample['wall_seconds'] * 1000:.6f} |")
    if "error" in result:
        lines += ["", result["error"]]
    common.write_atomic(args.report or args.output.with_suffix(".md"), "\n".join(lines) + "\n")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--counts", type=positive_csv)
    parser.add_argument("--iterations", type=int, default=32)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--probe-binary", type=pathlib.Path)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path)
    args = parser.parse_args(argv)
    counts = args.counts or ([256, 4096] if args.profile == "smoke" else [256, 4096, 16384])
    if min(args.iterations, args.repetitions) < 1:
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
    with tempfile.TemporaryDirectory(prefix="casita-memory-snapshots-") as temporary:
        work = pathlib.Path(temporary)
        result = {"schema_version": 1, "result_schema": "casita.memory-snapshots.v1", "suite_id": "state-and-publication",
                  "complete": False, "environment": common.environment_metadata(work),
                  "configuration": {"profile": args.profile, "counts": counts, "iterations": args.iterations, "repetitions": args.repetitions},
                  "artifacts": [{"path": str(binary), "sha256": digest}], "samples": [], "processes": []}
        save(args, result)
        schedule = [(rep, count, variant) for rep in range(1, args.repetitions + 1)
                    for count in counts for variant in ("shared", "deep-copy")]
        random.Random(0xCA517A).shuffle(schedule)
        try:
            for repetition, count, variant in schedule:
                print(f"memory-snapshots: {count} entries, {variant}, repetition {repetition}", flush=True)
                env = {**os.environ, "CASITA_SNAPSHOT_COUNT": str(count), "CASITA_SNAPSHOT_ITERATIONS": str(args.iterations), "CASITA_SNAPSHOT_VARIANT": variant}
                stdout, stderr = work / "stdout", work / "stderr"
                timing = common.measured_command(common.CommandSpec(
                    [[str(binary), PROBE, "--exact", "--ignored", "--nocapture"]], work, env), stdout, stderr, check=False)
                captured = stdout.read_text()
                result["processes"].append({**timing, "count": count, "variant": variant, "repetition": repetition, "stdout": captured, "stderr": stderr.read_text()})
                cases = [json.loads(line.removeprefix("memory_snapshot_sample ")) for line in captured.splitlines() if line.startswith("memory_snapshot_sample ")]
                if timing["exit_code"] != 0 or "test result: ok. 1 passed; 0 failed;" not in captured or len(cases) != 1:
                    raise common.BenchmarkError(f"expected one passing probe and one case: {captured}\n{stderr.read_text()}")
                case = cases[0]
                if any(case.get(key) != value for key, value in {"count": count, "variant": variant, "iterations": args.iterations, "correctness": CORRECTNESS}.items()):
                    raise common.BenchmarkError("configuration mismatch or missing correctness gate")
                for phase in ("snapshot", "publish"):
                    nanos = case.get(f"{phase}_nanos")
                    if type(nanos) is not int or nanos < 0:
                        raise common.BenchmarkError("invalid timing")
                    result["samples"].append({"status": "ok", "operation": phase, "entries": count, "variant": variant,
                        "repetition": repetition, "wall_seconds": nanos / 1e9, "max_rss_bytes": timing["max_rss_bytes"], "correctness": CORRECTNESS})
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
