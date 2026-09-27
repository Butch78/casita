"""Retained-history, physical pack-cache, and combined network scale matrices."""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import pathlib
import random
import subprocess
import tempfile

from benchmarks.suites import repository as common
from benchmarks.suites.pack.catalog import build_probe_binary
from benchmarks.suites.transfer import s3_path

PROBES = {
    "history": "scale_benchmarks::benchmark_retained_history",
    "pack-cache": "scale_benchmarks::benchmark_pack_cache_working_set",
}
SNAPSHOT_METRICS = {
    "catalog_snapshot_calls", "catalog_snapshot_nanos",
    "catalog_snapshot_payload_bytes_lower_bound", "catalog_snapshot_accounting_nanos",
}
PUBLICATION_PHASES = {
    "coordination_wait", "snapshot", "validation", "payload_prepare", "state_commit", "payload_finish",
}
OPERATIONS = {f"{pattern}-{phase}" for pattern in ("sequential", "random", "skewed") for phase in ("cold", "warm")}


def fingerprint(path):
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def positive_csv(value):
    try:
        values = [int(part) for part in value.split(",")]
    except ValueError as error:
        raise argparse.ArgumentTypeError("expected comma-separated integers") from error
    if not values or min(values) < 1 or len(set(values)) != len(values):
        raise argparse.ArgumentTypeError("counts must be positive and unique")
    return values


def build_parser():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=("history", "pack-cache", "network"), default="history")
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--probe-binary", type=pathlib.Path)
    parser.add_argument("--helper", type=pathlib.Path)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--generations", type=positive_csv)
    parser.add_argument("--window", type=int, default=100)
    parser.add_argument("--idle-sessions", type=int, default=0, help="diagnostic mutation starts without publishing after each history audit")
    parser.add_argument("--seed-batch-size", type=int, default=64, help="history setup batch; 1 measures every sequential publication")
    parser.add_argument("--cache-mib", type=int)
    parser.add_argument("--working-set-kib", type=positive_csv)
    parser.add_argument("--reads", type=int)
    parser.add_argument("--rtt-ms", default="0,25,100")
    parser.add_argument("--bandwidths-kib", default="0,1024,8192", help="per connection, each direction; 0 means unlimited")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    parser.add_argument("--report", type=pathlib.Path)
    return parser


def parse_samples(stdout, mode, points=None):
    samples = [json.loads(line.removeprefix("scale_sample ")) for line in stdout.splitlines() if line.startswith("scale_sample ")]
    if mode == "history":
        expected = {(operation, count) for count in points for operation in ("tiny-delta-publication", "reopen")}
        actual = [(sample.get("operation"), sample.get("generations")) for sample in samples]
    else:
        expected = OPERATIONS
        actual = [sample.get("operation") for sample in samples]
    if set(actual) != expected or len(actual) != len(expected):
        raise common.BenchmarkError(f"missing or duplicate scale cases: expected {expected}, received {actual}")
    for sample in samples:
        if sample.get("status") != "ok" or not isinstance(sample.get("wall_seconds"), (int, float)) or not math.isfinite(sample["wall_seconds"]) or sample["wall_seconds"] < 0:
            raise common.BenchmarkError("scale probe emitted invalid/failed measurements")
        # Older probe binaries lack snapshot accounting. When present, require
        # a complete set of integer counters for every update in this sample.
        updates = sample.get("updates", [])
        if mode == "history" and any(SNAPSHOT_METRICS & update.keys() for update in updates):
            for update in updates:
                if any(type(update.get(field)) is not int or update[field] < 0 for field in SNAPSHOT_METRICS):
                    raise common.BenchmarkError("incomplete or invalid publication snapshot metrics")
        if mode == "history" and any("publication_phases" in update for update in updates):
            if type(sample.get("coordinates_payload_catalog")) is not bool:
                raise common.BenchmarkError("missing publication catalog coordination mode")
            for update in updates:
                phases = update.get("publication_phases")
                if not isinstance(phases, dict) or phases.keys() != PUBLICATION_PHASES:
                    raise common.BenchmarkError("incomplete publication phases")
                for metrics in phases.values():
                    if not isinstance(metrics, dict) or any(type(metrics.get(field)) is not int or metrics[field] < 0 for field in ("calls", "nanos")):
                        raise common.BenchmarkError("invalid publication phase metrics")
                    if not metrics["calls"] and metrics["nanos"]:
                        raise common.BenchmarkError("unexecuted publication phase has elapsed time")
                if type(update.get("publish_nanos")) is not int or sum(m["nanos"] for m in phases.values()) > update["publish_nanos"]:
                    raise common.BenchmarkError("publication phases exceed enclosing publish time")
                for field in ("catalog_build_calls", "catalog_build_nanos"):
                    if type(update.get(field)) is not int or update[field] < 0:
                        raise common.BenchmarkError("invalid catalog build metrics")
    if "1 passed" not in stdout:
        raise common.BenchmarkError("scale probe did not execute one passing test")
    return samples


def native(args, work, result):
    binary = (args.probe_binary or build_probe_binary()).resolve()
    if not binary.is_file():
        raise common.BenchmarkError(f"missing probe binary: {binary}")
    result["artifacts"] = [{"path": str(binary), "sha256": fingerprint(binary)}]
    points = args.generations or ([10, 200] if args.profile == "smoke" else [100, 1000, 10000])
    if points != sorted(points):
        raise common.BenchmarkError("generation checkpoints must be strictly increasing")
    cache_mib = args.cache_mib or (1 if args.profile == "smoke" else 8)
    working = args.working_set_kib or ([512, 4096] if args.profile == "smoke" else [4096, 32768])
    if args.mode == "pack-cache" and (min(working) >= cache_mib * 1024 or max(working) <= cache_mib * 1024):
        raise common.BenchmarkError("working sets must include both below and above cache capacity")
    if args.mode == "pack-cache" and any(value < 128 or value % 64 for value in working):
        raise common.BenchmarkError("working sets must be >=128 KiB and multiples of 64 KiB")
    schedule = [(rep, size) for rep in range(1, args.repetitions + 1) for size in ([None] if args.mode == "history" else working)]
    random.Random(0xCA517A).shuffle(schedule)
    if args.mode == "history":
        result["configuration"].update(generations=points, layout="sequential" if args.seed_batch_size == 1 else "batched-seed")
    else:
        result["configuration"].update(cache_mib=cache_mib, working_set_kib=working)
    for index, (repetition, size) in enumerate(schedule):
        env = {**os.environ, "CASITA_SCALE_GENERATIONS": ",".join(map(str, points)), "CASITA_SCALE_WINDOW": str(args.window), "CASITA_SCALE_SEED_BATCH": str(args.seed_batch_size),
               "CASITA_SCALE_CACHE_BYTES": str(cache_mib * 1024 * 1024),
               "CASITA_SCALE_IDLE_SESSIONS": str(args.idle_sessions)}
        if size is not None:
            env["CASITA_SCALE_WORKING_BYTES"] = str(size * 1024)
        env.pop("CASITA_SCALE_READS", None)
        if args.reads is not None:
            if size is not None and args.reads < size // 64:
                raise common.BenchmarkError("reads must cover at least one complete working set")
            env["CASITA_SCALE_READS"] = str(args.reads)
        print(f"{args.mode}: repetition {repetition}, working set {size} KiB", flush=True)
        stdout, stderr = work / f"native-{index}.stdout", work / f"native-{index}.stderr"
        timing = common.measured_command(common.CommandSpec([[str(binary), PROBES[args.mode], "--exact", "--ignored", "--nocapture"]], work, env), stdout, stderr, check=False)
        captured = stdout.read_text()
        process = {**timing, "repetition": repetition, "working_set_kib": size, "stdout": captured, "stderr": stderr.read_text()}
        result["processes"].append(process)
        if timing["exit_code"] != 0:
            # Keep already-audited checkpoints when a later frontier fails.
            partial = [json.loads(line.removeprefix("scale_sample ")) for line in captured.splitlines() if line.startswith("scale_sample ")]
            result["samples"].extend({**sample, "repetition": repetition, "process_max_rss_bytes": timing["max_rss_bytes"]} for sample in partial)
            save(args, result)
            raise common.BenchmarkError(f"{args.mode} probe failed ({timing['exit_code']}): {process['stderr']}")
        measured = parse_samples(captured, args.mode, points)
        for sample in measured:
            sample.update(repetition=repetition, process_max_rss_bytes=timing["max_rss_bytes"])
        result["samples"].extend(measured)
        save(args, result)


def nonnegative_csv(value):
    values = [int(part) for part in value.split(",")]
    if not values or min(values) < 0 or len(set(values)) != len(values):
        raise common.BenchmarkError("network dimensions must be unique nonnegative integers")
    return values


def network(args, work, result):
    rtts = nonnegative_csv(args.rtt_ms)
    rates = nonnegative_csv(args.bandwidths_kib)
    cache_mib = args.cache_mib or 8
    files = 8 if args.profile == "smoke" else 64
    helper = (args.helper or pathlib.Path("target/release/examples/s3_path_transfer")).resolve()
    if not args.no_build:
        subprocess.run(["cargo", "build", "--release", "--example", "s3_path_transfer", "--features", "s3,ssh,experimental"], check=True)
    if not helper.is_file():
        raise common.BenchmarkError(f"missing transfer helper: {helper}")
    result["artifacts"] = [{"path": str(helper), "sha256": fingerprint(helper)}]
    schedule = [(rep, rate) for rep in range(1, args.repetitions + 1) for rate in rates]
    random.Random(0xCA517A).shuffle(schedule)
    result["configuration"].update(rtt_ms=rtts, bandwidths_kib=rates, cache_mib=cache_mib, subtree_files=files, file_kib=16, path_depth=4,
        destination="fresh in-memory repository per phase", shaping="per connection and per direction; not an aggregate network link")
    for index, (repetition, rate) in enumerate(schedule):
        output = work / f"network-{index}.json"
        command = ["--helper", str(helper), "--no-build", "--depths", "4", "--subtree-files", str(files),
            "--file-kib", "16", "--cache-mib", str(cache_mib), "--rtt-ms", ",".join(map(str, rtts)), "--bandwidth-kib", str(rate),
            "--transports", "direct-s3,atomic-rpc", "--repetitions", "1", "--output", str(output), "--report", str(output.with_suffix(".md"))]
        print(f"network: repetition {repetition}, {rate} KiB/s per connection", flush=True)
        if s3_path.main(command):
            raise common.BenchmarkError("network transfer matrix failed")
        measured = json.loads(output.read_text())
        expected = {(transport, rtt) for transport in ("direct-s3", "atomic-rpc") for rtt in rtts}
        actual = [(sample["transport"], sample["rtt_ms"]) for sample in measured["samples"]]
        if set(actual) != expected or len(actual) != len(expected):
            raise common.BenchmarkError("network matrix is incomplete or contains duplicate cases")
        for sample in measured["samples"]:
            for phase in ("cold", "warm"):
                metrics = sample[phase]
                result["samples"].append({"status": "ok", "implementation": "casita", "operation": f"{sample['transport']}-{phase}",
                    "repetition": repetition, "rtt_ms": sample["rtt_ms"], "bandwidth_kib_per_connection": rate,
                    "cache_bytes": cache_mib * 1024 * 1024, "subtree_files": files, "file_bytes": 16 * 1024, "path_depth": 4,
                    "wall_seconds": metrics["wall_nanos"] / 1e9, "metrics": metrics, "process_resources": sample.get("process_resources"),
                    "correctness": "verified selected closure + installed destination root; fresh in-memory destination per phase"})
        result["processes"].append(measured)
        save(args, result)


def save(args, result):
    common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
    lines = ["# Scale benchmark", "", "Timings are local measurements, not controlled revision comparisons. Process RSS includes fixture setup.", "", f"Complete: {result['complete']}." + (f" Error: {result['error']}" if "error" in result else ""), "",
        "| Operation | Generations / working bytes / RTT | Bandwidth KiB/s | Repetition | Mean/op ms | p95/op ms | Backend pack bytes |", "|---|---:|---:|---:|---:|---:|---:|"]
    for sample in result["samples"]:
        scale = sample.get("generations", sample.get("working_set_bytes", sample.get("rtt_ms", "")))
        p95 = f"{sample['p95_nanos'] / 1e6:.4f}" if "p95_nanos" in sample else ""
        lines.append(f"| {sample['operation']} | {scale} | {sample.get('bandwidth_kib_per_connection', '')} | {sample['repetition']} | {sample['wall_seconds'] * 1000 / sample.get('operations', 1):.4f} | {p95} | {sample.get('backend_read_bytes', '')} |")
    common.write_atomic(args.report or args.output.with_suffix(".md"), "\n".join(lines) + "\n")


def main(argv=None):
    args = build_parser().parse_args(argv)
    if not 1 <= args.seed_batch_size <= 128:
        raise common.BenchmarkError("seed batch size must be between 1 and 128")
    if args.repetitions < 1 or args.window < 1 or args.idle_sessions < 0 or (args.cache_mib is not None and args.cache_mib < 1) or (args.reads is not None and args.reads < 1):
        raise common.BenchmarkError("counts and cache capacity must be positive")
    if args.no_build and args.mode != "network" and args.probe_binary is None:
        raise common.BenchmarkError("--no-build requires --probe-binary")
    with tempfile.TemporaryDirectory(prefix="casita-scale-") as temporary:
        work = pathlib.Path(temporary)
        result = {"schema_version": 1, "result_schema": "casita.scale.v1", "suite_id": "transfer" if args.mode == "network" else "huge-repositories",
            "environment": common.environment_metadata(work), "configuration": {key: str(value) if isinstance(value, pathlib.Path) else value for key, value in vars(args).items()},
            "complete": False, "samples": [], "processes": []}
        save(args, result)
        try:
            (network if args.mode == "network" else native)(args, work, result)
        except Exception as error:
            result["error"] = str(error)
            save(args, result)
            raise
        result["complete"] = True
        save(args, result)
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
