"""Local object-scoped and snapshot-wide reader admission, payload reads, release, and GC."""
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

PROBE = "object_read_tests::benchmark_object_reads"
CORRECTNESS = "exact bytes and seek after GC, expected logical and physical collection, no leaked pins"
PHASES = ("open", "admission", "resolve", "handoff", "read", "release", "gc")


def sizes_csv(value):
    try:
        values = [int(part) for part in value.split(",")]
    except ValueError as error:
        raise argparse.ArgumentTypeError("expected comma-separated sizes") from error
    if not values or min(values) < 0 or len(set(values)) != len(values):
        raise argparse.ArgumentTypeError("sizes must be nonnegative and unique")
    return values


def parse_sample(stdout, size, garbage, mode, admission="cold"):
    try:
        cases = [json.loads(line.removeprefix("object_read_sample "))
                 for line in stdout.splitlines() if line.startswith("object_read_sample ")]
    except json.JSONDecodeError as error:
        raise common.BenchmarkError("invalid object-read JSON") from error
    if "test result: ok. 1 passed; 0 failed;" not in stdout or len(cases) != 1:
        raise common.BenchmarkError("object-read probe must execute one passing test and emit one case")
    case = cases[0]
    if (not isinstance(case, dict) or case.get("size") != size or case.get("garbage") != garbage
            or case.get("mode") != mode or case.get("reader_admission") != admission
            or case.get("correctness") != CORRECTNESS):
        raise common.BenchmarkError("wrong object-read configuration or missing correctness gate")
    metrics = [*(f"{phase}_nanos" for phase in PHASES), "open_ledger_updates", "release_ledger_updates",
               "gc_removed", "pack_bytes_before", "pack_bytes_after",
               "open_durable_revision_changed", "release_durable_revision_changed"]
    if any(type(case.get(key)) is not int or case[key] < 0 for key in metrics):
        raise common.BenchmarkError("missing or invalid object-read metric")
    if case["gc_removed"] != (0 if mode == "snapshot" else garbage):
        raise common.BenchmarkError("incorrect logical collection")
    if mode != "snapshot" and case["pack_bytes_after"] >= case["pack_bytes_before"]:
        raise common.BenchmarkError("object reader prevented physical GC progress")
    if (case["open_durable_revision_changed"] != int(mode == "durable-object" or admission == "cold")
            or case["release_durable_revision_changed"] != int(mode == "durable-object")):
        raise common.BenchmarkError("incorrect durable reader update count")
    return case


def save(args, result):
    common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
    lines = ["# Local reader protection", "", f"Complete: {result['complete']}", "",
             "Object and snapshot modes have different retention contracts. Both verify bytes and seek after independent GC/vacuum.",
             "Object and durable-object share the same narrow retention contract; durable-object is a test-only control. Cold means first reader owner, not cold OS caches.",
             "Admission includes metadata snapshot/lookup and pin registration. Resolve builds the physical plan (or eagerly reads a bare chunk).",
             "Open contains admission and resolve; do not sum these overlapping timings. Handoff flush drains the temporary snapshot lease.",
             "Read measures stream consumption after GC with pack cache disabled; OS caches are not flushed. Release includes flush.",
             "Fixture creation, root removal, audits and vacuum are excluded from phase timings; process RSS includes them.", "",
             "| Bytes | Garbage objects | Mode | Admission | Phase | Repetition | ms |", "|---:|---:|---|---|---|---:|---:|"]
    if "error" in result:
        lines += [f"Error: {result['error']}", ""]
    for sample in result["samples"]:
        lines.append(f"| {sample['file_bytes']} | {sample['entries']} | {sample['variant']} | {sample['reader_admission']} | {sample['operation']} | {sample['repetition']} | {sample['wall_seconds'] * 1000:.3f} |")
    common.write_atomic(args.report or args.output.with_suffix(".md"), "\n".join(lines) + "\n")


def choices_csv(choices):
    def parse(value):
        values = value.split(",")
        if not values or len(set(values)) != len(values) or set(values) - set(choices):
            raise argparse.ArgumentTypeError("expected unique choices from " + ",".join(choices))
        return values
    return parse


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--sizes", type=sizes_csv)
    parser.add_argument("--modes", type=choices_csv(("object", "durable-object", "snapshot")),
                        default=["object", "durable-object", "snapshot"])
    parser.add_argument("--admissions", type=choices_csv(("cold", "warm")), default=["cold", "warm"])
    parser.add_argument("--garbage-counts", type=positive_csv)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--probe-binary", type=pathlib.Path)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path)
    args = parser.parse_args(argv)
    # Include both sides of the 64 KiB inline verification threshold, plus
    # empty, tiny bare-chunk, and guaranteed multi-chunk payloads.
    sizes = args.sizes if args.sizes is not None else [0, 64, 65535, 65536, 65537, 1048593]
    counts = args.garbage_counts or ([1, 4] if args.profile == "smoke" else [1, 16, 64])
    if args.repetitions < 1:
        parser.error("repetitions must be positive")
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
    with tempfile.TemporaryDirectory(prefix="casita-object-reads-") as temporary:
        work = pathlib.Path(temporary)
        result = {"schema_version": 1, "result_schema": "casita.object-reads.v1", "suite_id": "repository-e2e",
                  "complete": False, "environment": common.environment_metadata(work),
                  "configuration": {"profile": args.profile, "sizes": sizes, "garbage_counts": counts,
                                    "modes": args.modes, "admissions": args.admissions, "repetitions": args.repetitions, "pack_cache_bytes": 0, "pack_target_bytes": 131072},
                  "artifacts": [{"path": str(binary), "sha256": digest}], "samples": [], "processes": []}
        save(args, result)
        schedule = [(rep, size, count, mode, admission) for rep in range(1, args.repetitions + 1)
                    for size in sizes for count in counts for mode in args.modes for admission in args.admissions]
        random.Random(0xCA517A).shuffle(schedule)
        try:
            for repetition, size, count, mode, admission in schedule:
                print(f"object-reads: {size} bytes, {count} garbage, {mode}, {admission}, repetition {repetition}", flush=True)
                env = {**os.environ, "CASITA_READ_SIZE": str(size), "CASITA_READ_GARBAGE": str(count), "CASITA_READ_MODE": mode, "CASITA_READ_ADMISSION": admission}
                stdout, stderr = work / "stdout", work / "stderr"
                timing = common.measured_command(common.CommandSpec(
                    [[str(binary), PROBE, "--exact", "--ignored", "--nocapture"]], work, env), stdout, stderr, check=False)
                captured = stdout.read_text()
                result["processes"].append({**timing, "size": size, "garbage": count, "mode": mode, "reader_admission": admission, "repetition": repetition,
                                            "stdout": captured, "stderr": stderr.read_text()})
                if timing["exit_code"] != 0:
                    raise common.BenchmarkError(f"object-read probe failed: {captured}\n{stderr.read_text()}")
                case = parse_sample(captured, size, count, mode, admission)
                for phase in PHASES:
                    result["samples"].append({"status": "ok", "operation": phase, "entries": count,
                        "file_bytes": size, "variant": mode, "reader_admission": admission, "repetition": repetition,
                        "wall_seconds": case[f"{phase}_nanos"] / 1e9, "max_rss_bytes": timing["max_rss_bytes"],
                        "metrics": {key: case[key] for key in ("open_ledger_updates", "release_ledger_updates", "gc_removed", "pack_bytes_before", "pack_bytes_after", "open_durable_revision_changed", "release_durable_revision_changed")},
                        "correctness": CORRECTNESS})
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
