"""Compare ordered and ready-first filesystem ingestion with controlled I/O delays."""
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

PROBE = "filesystem::scheduling_tests::benchmark_ingest_scheduling"
CORRECTNESS = "exact post-order paths, sizes and digests; bounded active reads and pages"


def pattern_csv(value):
    patterns = value.split(",")
    if not patterns or len(patterns) != len(set(patterns)) or any(p not in ("uniform", "skewed") for p in patterns):
        raise argparse.ArgumentTypeError("expected uniform, skewed, or uniform,skewed")
    return patterns


def parse_sample(stdout, files, concurrency, pattern, mode):
    try:
        cases = [json.loads(line.removeprefix("ingest_scheduling_sample "))
                 for line in stdout.splitlines() if line.startswith("ingest_scheduling_sample ")]
    except json.JSONDecodeError as error:
        raise common.BenchmarkError("invalid ingestion scheduling JSON") from error
    if len(cases) != 1 or "test result: ok. 1 passed; 0 failed;" not in stdout:
        raise common.BenchmarkError("scheduling probe must execute exactly one passing case")
    case = cases[0]
    if (not isinstance(case, dict) or case.get("files") != files
            or case.get("concurrency") != concurrency or case.get("pattern") != pattern
            or case.get("mode") != mode or case.get("page_limit") != 1024
            or case.get("correctness") != CORRECTNESS
            or type(case.get("wall_nanos")) is not int or case["wall_nanos"] <= 0
            or type(case.get("peak_active")) is not int or not 1 <= case["peak_active"] <= concurrency
            or type(case.get("largest_page")) is not int or not 1 <= case["largest_page"] <= 1024):
        raise common.BenchmarkError("wrong scheduling configuration or missing correctness gate")
    return case


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--counts", type=positive_csv, default=[15, 16, 17, 64, 1023, 1024, 1025])
    parser.add_argument("--concurrency", type=positive_csv)
    parser.add_argument("--patterns", type=pattern_csv, default=["uniform", "skewed"])
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--probe-binary", type=pathlib.Path)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path)
    parser.add_argument("--measurement-note", default="")
    args = parser.parse_args(argv)
    if args.repetitions < 1:
        parser.error("repetitions must be positive")
    if args.no_build and args.probe_binary is None:
        parser.error("--no-build requires --probe-binary")
    concurrency = args.concurrency or ([1, 16] if args.profile == "smoke" else [1, 16, 32])
    binary = args.probe_binary
    if binary is None:
        built = subprocess.run(["cargo", *CARGO_ARGUMENTS], cwd=cli.ROOT, capture_output=True, text=True)
        if built.returncode:
            raise common.BenchmarkError(built.stderr or built.stdout)
        binary = parse_probe_binary(built.stdout)
    binary = binary.resolve()
    with binary.open("rb") as handle:
        digest = hashlib.file_digest(handle, "sha256").hexdigest()
    with tempfile.TemporaryDirectory(prefix="casita-ingest-scheduling-") as temporary:
        work = pathlib.Path(temporary)
        result = dict(schema_version=1, result_schema="casita.ingest-scheduling.v1",
                      suite_id="repository-e2e", complete=False,
                      environment=common.environment_metadata(work),
                      configuration=dict(profile=args.profile, files=args.counts, concurrency=concurrency, patterns=args.patterns,
                                         repetitions=args.repetitions, measurement_note=args.measurement_note,
                                         skew="20 ms before every 16th file, 1 ms before others; uniform has no injected delay",
                                         timing="walk, bounded file reads and hashing; fixture setup and validation excluded; no repository publication"),
                      artifacts=[dict(path=str(binary), sha256=digest)], samples=[], processes=[])

        def save():
            common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
            lines = ["# Ingest scheduling", "", f"Complete: {result['complete']}", "",
                     result["configuration"]["timing"], "", result["configuration"]["skew"], "",
                     args.measurement_note, "", "| Files | Concurrency | Pattern | Mode | Repetition | Seconds | Peak reads |",
                     "|---:|---:|---|---|---:|---:|---:|"]
            for row in result["samples"]:
                lines.append(f"| {row['entries']} | {row['concurrency']} | {row['pattern']} | {row['variant']} | {row['repetition']} | {row['wall_seconds']:.6f} | {row['peak_active']} |")
            if "error" in result:
                lines += ["", "Error: " + result["error"]]
            common.write_atomic(args.report or args.output.with_suffix(".md"), "\n".join(lines) + "\n")

        schedule = [(rep, count, limit, pattern, mode) for rep in range(args.repetitions)
                    for count in args.counts for limit in concurrency
                    for pattern in args.patterns for mode in ("ordered", "ready")]
        random.Random(1729).shuffle(schedule)
        save()
        try:
            for repetition, files, limit, pattern, mode in schedule:
                print(f"ingest-scheduling: {files} files, concurrency={limit}, {pattern}, {mode}, repetition={repetition}", flush=True)
                env = {**os.environ, "CASITA_INGEST_FILES": str(files), "CASITA_INGEST_CONCURRENCY": str(limit),
                       "CASITA_INGEST_PATTERN": pattern, "CASITA_INGEST_MODE": mode}
                stdout, stderr = work / "stdout", work / "stderr"
                timing = common.measured_command(common.CommandSpec(
                    [[str(binary), PROBE, "--exact", "--ignored", "--nocapture"]], work, env), stdout, stderr, check=False)
                captured = stdout.read_text()
                result["processes"].append({**timing, "files": files, "concurrency": limit, "pattern": pattern,
                                            "mode": mode, "repetition": repetition, "stdout": captured, "stderr": stderr.read_text()})
                if timing["exit_code"] != 0:
                    raise common.BenchmarkError(f"scheduling probe failed: {captured}\n{stderr.read_text()}")
                case = parse_sample(captured, files, limit, pattern, mode)
                result["samples"].append(dict(status="ok", implementation="casita", operation="walk-read-hash",
                    entries=files, concurrency=limit, pattern=pattern, variant=mode, repetition=repetition,
                    wall_seconds=case["wall_nanos"] / 1e9, peak_active=case["peak_active"], largest_page=case["largest_page"],
                    max_rss_bytes=timing["max_rss_bytes"], correctness=CORRECTNESS))
                save()
        except Exception as error:
            result["error"] = str(error)
            save()
            raise
        result["complete"] = True
        save()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
