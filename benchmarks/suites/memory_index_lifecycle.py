"""Memory index read, collection and destruction scaling."""
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

PROBE = "metadata::benchmarks::benchmark_memory_index_lifecycle"
CORRECTNESS = "exact lookup and paginated order; retained inventory, roots, validation and births; original snapshot unchanged"


def save(args, result):
    common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
    lines = ["# Memory index lifecycle", "", f"Complete: {result['complete']}", "",
             "Separate seed, all-key lookup, paginated scan, collection and final-owner drop timings; construction and post-collection audits excluded.",
             "Held mode retains the seeded revision during collection; none mode releases it first.",
             "Lookup includes object and application-record checks; scan uses 128-record pages. Collection removes 0, 1, 50, 99 or 100 percent. RSS includes seeding.", "",
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
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--probe-binary", type=pathlib.Path)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path)
    args = parser.parse_args(argv)
    counts = args.counts or ([256, 4096] if args.profile == "smoke" else [256, 4096, 16384])
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
    with tempfile.TemporaryDirectory(prefix="casita-memory-index-lifecycle-") as temporary:
        work = pathlib.Path(temporary)
        result = {"schema_version": 1, "result_schema": "casita.memory-index-lifecycle.v1", "suite_id": "state-and-publication",
                  "complete": False, "environment": common.environment_metadata(work),
                  "configuration": {"profile": args.profile, "counts": counts, "repetitions": args.repetitions},
                  "artifacts": [{"path": str(binary), "sha256": digest}], "samples": [], "processes": []}
        save(args, result)
        schedule = [(rep, count, percent, readers) for rep in range(1, args.repetitions + 1)
                    for count in counts for percent in (0, 1, 50, 99, 100) for readers in ("none", "held")]
        random.Random(0xCA517A).shuffle(schedule)
        try:
            for repetition, count, percent, readers in schedule:
                variant = f"remove-{percent}/{readers}"
                print(f"memory-index-lifecycle: {count} entries, {variant}, repetition {repetition}", flush=True)
                env = {**os.environ, "CASITA_INDEX_COUNT": str(count), "CASITA_INDEX_REMOVE_PERCENT": str(percent), "CASITA_INDEX_READERS": readers}
                stdout, stderr = work / "stdout", work / "stderr"
                timing = common.measured_command(common.CommandSpec(
                    [[str(binary), PROBE, "--exact", "--ignored", "--nocapture"]], work, env), stdout, stderr, check=False)
                captured = stdout.read_text()
                result["processes"].append({**timing, "count": count, "variant": variant, "repetition": repetition, "stdout": captured, "stderr": stderr.read_text()})
                cases = [json.loads(line.removeprefix("memory_index_sample ")) for line in captured.splitlines() if line.startswith("memory_index_sample ")]
                if timing["exit_code"] != 0 or "test result: ok. 1 passed; 0 failed;" not in captured or len(cases) != 1:
                    raise common.BenchmarkError(f"expected one passing probe and one case: {captured}\n{stderr.read_text()}")
                case = cases[0]
                if any(case.get(key) != value for key, value in {"count": count, "remove_percent": percent, "readers": readers, "correctness": CORRECTNESS}.items()):
                    raise common.BenchmarkError("configuration mismatch or missing correctness gate")
                for phase in ("seed", "lookup", "scan", "collection", "drop"):
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
