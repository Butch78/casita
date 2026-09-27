"""Query-only snapshot connection reuse across the bounded idle-cache limit."""
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

PROBE = "sqlite::tests::benchmark_snapshot_connections"
CORRECTNESS = "fresh committed generation, query-only, bounded idle cache, no idle transaction"


def parse_sample(stdout, width, iterations, mode):
    try:
        cases = [json.loads(line.removeprefix("snapshot_connection_sample "))
                 for line in stdout.splitlines() if line.startswith("snapshot_connection_sample ")]
    except json.JSONDecodeError as error:
        raise common.BenchmarkError("invalid snapshot connection JSON") from error
    if len(cases) != 1 or "test result: ok. 1 passed; 0 failed;" not in stdout:
        raise common.BenchmarkError("snapshot probe must execute exactly one passing case")
    case = cases[0]
    if (not isinstance(case, dict) or case.get("width") != width
            or case.get("iterations") != iterations or case.get("mode") != mode
            or case.get("idle_limit") != 8 or case.get("correctness") != CORRECTNESS
            or type(case.get("wall_nanos")) is not int or case["wall_nanos"] <= 0):
        raise common.BenchmarkError("wrong snapshot configuration or missing correctness gate")
    return case


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--widths", type=positive_csv, default=[1, 7, 8, 9, 16])
    parser.add_argument("--iterations", type=int)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--probe-binary", type=pathlib.Path)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path)
    parser.add_argument("--measurement-note", default="")
    args = parser.parse_args(argv)
    iterations = args.iterations if args.iterations is not None else (8 if args.profile == "smoke" else 256)
    if min(iterations, args.repetitions) < 1:
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
    with tempfile.TemporaryDirectory(prefix="casita-snapshot-connections-") as temporary:
        work = pathlib.Path(temporary)
        result = dict(schema_version=1, result_schema="casita.snapshot-connections.v1",
                      suite_id="state-and-publication", complete=False,
                      environment=common.environment_metadata(work),
                      configuration=dict(profile=args.profile, widths=args.widths, iterations=iterations,
                                         repetitions=args.repetitions, idle_limit=8,
                                         measurement_note=args.measurement_note,
                                         timing="acquire/query/release burst; fresh control also destroys idle connections; writer commits excluded"),
                      artifacts=[dict(path=str(binary), sha256=digest)], samples=[], processes=[])

        def save():
            common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
            lines = ["# Snapshot connections", "", f"Complete: {result['complete']}", "",
                     result["configuration"]["timing"], "", args.measurement_note, "",
                     "| Live readers | Mode | Repetition | microseconds/snapshot |",
                     "|---:|---|---:|---:|"]
            for sample in result["samples"]:
                lines.append(f"| {sample['entries']} | {sample['variant']} | {sample['repetition']} | {sample['wall_seconds'] * 1e6:.2f} |")
            if "error" in result:
                lines += ["", "Error: " + result["error"]]
            common.write_atomic(args.report or args.output.with_suffix(".md"), "\n".join(lines) + "\n")

        save()
        schedule = [(rep, width, mode) for rep in range(1, args.repetitions + 1)
                    for width in args.widths for mode in ("fresh", "reused")]
        random.Random(0xCA517A).shuffle(schedule)
        try:
            for repetition, width, mode in schedule:
                print(f"snapshot-connections: width {width}, {mode}, repetition {repetition}", flush=True)
                env = {**os.environ, "CASITA_SNAPSHOT_WIDTH": str(width),
                       "CASITA_SNAPSHOT_ITERATIONS": str(iterations), "CASITA_SNAPSHOT_MODE": mode}
                stdout, stderr = work / "stdout", work / "stderr"
                timing = common.measured_command(common.CommandSpec(
                    [[str(binary), PROBE, "--exact", "--ignored", "--nocapture"]], work, env), stdout, stderr, check=False)
                captured = stdout.read_text()
                result["processes"].append({**timing, "width": width, "mode": mode, "repetition": repetition,
                                            "stdout": captured, "stderr": stderr.read_text()})
                if timing["exit_code"] != 0:
                    raise common.BenchmarkError(f"snapshot probe failed: {captured}\n{stderr.read_text()}")
                case = parse_sample(captured, width, iterations, mode)
                result["samples"].append(dict(status="ok", operation="snapshot-acquire-query-release",
                    entries=width, variant=mode, repetition=repetition,
                    wall_seconds=case["wall_nanos"] / 1e9 / iterations / width,
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
