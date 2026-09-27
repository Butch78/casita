"""Separate filesystem roots with shared-session single walks versus a multi-root traversal."""
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
from benchmarks.suites.metadata_collection import positive_csv
from benchmarks.suites.pack.catalog import parse_probe_binary


PROBE = "scale_benchmarks::benchmark_filesystem_outputs"
CORRECTNESS = "independent tree digests, exact roots, byte-for-byte payload reads, clean fsck"
CARGO_ARGUMENTS = ("test", "--release", "--features", "cli", "--lib", "--no-run", "--message-format=json")


def parse_sample(stdout: str, count: int, files: int, size: int, mode: str) -> dict:
    prefix = "filesystem_outputs_sample "
    try:
        samples = [json.loads(line.removeprefix(prefix)) for line in stdout.splitlines() if line.startswith(prefix)]
    except json.JSONDecodeError as error:
        raise common.BenchmarkError("invalid filesystem-outputs JSON") from error
    if "test result: ok. 1 passed; 0 failed;" not in stdout or len(samples) != 1:
        raise common.BenchmarkError("filesystem-outputs probe must execute one passing test and emit one sample")
    sample = samples[0]
    if not isinstance(sample, dict) or sample.get("operation") != "filesystem-outputs":
        raise common.BenchmarkError("filesystem-outputs sample has the wrong operation")
    if sample.get("mode") != mode or sample.get("outputs") != count or sample.get("file_bytes") != size or sample.get("files") != files:
        raise common.BenchmarkError("filesystem-outputs sample has the wrong configuration")
    if sample.get("correctness") != CORRECTNESS:
        raise common.BenchmarkError("filesystem-outputs correctness gate is missing")
    for field in ("nanos", "session_nanos", "stage_nanos", "publish_nanos", "maintenance_nanos", "traversal_nanos"):
        if type(sample.get(field)) is not int or sample[field] < 0:
            raise common.BenchmarkError(f"invalid filesystem-outputs timing field: {field}")
    if sample["nanos"] <= 0 or sample["logical_bytes"] != count * files * size:
        raise common.BenchmarkError("invalid filesystem-outputs timing or byte count")
    for field in ("pages", "publications", "walks"):
        if type(sample.get(field)) is not int or sample[field] < 1:
            raise common.BenchmarkError(f"invalid filesystem-outputs counter: {field}")
    if sample["walks"] != (1 if mode == "multi-root" else count):
        raise common.BenchmarkError("unexpected filesystem walk count")
    return sample


def save(args, result):
    common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
    if args.report:
        lines = ["# Filesystem output imports", "", "One mutation session in both modes. Fresh persistent repositories; fixture setup and correctness checks excluded.", "", "| Outputs | Files/root | Bytes/file | Mode | Rep | Total ms | Traverse/stage ms | Discovery ms | Publish ms | Maintenance ms | Pages | Publications |", "|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|"]
        for sample in result["samples"]:
            lines.append(f"| {sample['outputs']} | {sample['files']} | {sample['file_bytes']} | {sample['mode']} | {sample['repetition']} | {sample['nanos']/1e6:.3f} | {sample['stage_nanos']/1e6:.3f} | {sample['traversal_nanos']/1e6:.3f} | {sample['publish_nanos']/1e6:.3f} | {sample['maintenance_nanos']/1e6:.3f} | {sample['pages']} | {sample['publications']} |")
        common.write_atomic(args.report, "\n".join(lines) + "\n")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--counts", type=positive_csv)
    parser.add_argument("--files", type=positive_csv)
    parser.add_argument("--sizes", type=positive_csv)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--probe-binary", type=pathlib.Path)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path)
    args = parser.parse_args(argv)
    if args.repetitions < 1 or (args.no_build and args.probe_binary is None):
        parser.error("positive repetitions and a binary for --no-build are required")
    counts = args.counts or [2, 8, 32]
    sizes = args.sizes or [4096]
    files_per_root = args.files or ([1, 27, 28, 29, 30] if args.profile == "smoke" else [1, 27, 28, 29, 30, 123, 125, 126])
    binary = args.probe_binary
    if binary is None:
        built = subprocess.run(["cargo", *CARGO_ARGUMENTS], cwd=cli.ROOT, capture_output=True, text=True)
        if built.returncode:
            raise common.BenchmarkError(built.stderr or built.stdout)
        binary = parse_probe_binary(built.stdout)
    binary = binary.resolve()
    with binary.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    result = {
        "schema_version": 1,
        "result_schema": "casita.filesystem-outputs.v1",
        "suite_id": "state-and-publication",
        "complete": False,
        "environment": common.environment_metadata(cli.ROOT),
        "configuration": {"profile": args.profile, "counts": counts, "sizes": sizes, "files": files_per_root, "modes": ["per-output", "multi-root"], "repetitions": args.repetitions},
        "artifacts": [{"path": str(binary), "sha256": digest}],
        "samples": [],
        "processes": [],
    }
    save(args, result)
    jobs = [(repetition, count, files, size, mode) for repetition in range(1, args.repetitions + 1) for count in counts for files in files_per_root for size in sizes for mode in ("per-output", "multi-root")]
    random.Random(0xCA517A).shuffle(jobs)
    try:
        with tempfile.TemporaryDirectory(prefix="casita-filesystem-outputs-") as temporary:
            work = pathlib.Path(temporary)
            for repetition, count, files, size, mode in jobs:
                print(f"filesystem-outputs: outputs={count}, files={files}, bytes={size}, mode={mode}, repetition={repetition}", flush=True)
                stdout, stderr = work / "stdout", work / "stderr"
                env = {**os.environ, "CASITA_FS_OUTPUTS_COUNT": str(count), "CASITA_FS_OUTPUTS_FILES": str(files), "CASITA_FS_OUTPUTS_SIZE": str(size), "CASITA_FS_OUTPUTS_MODE": mode}
                timing = common.measured_command(common.CommandSpec([[str(binary), PROBE, "--exact", "--ignored", "--nocapture"]], work, env), stdout, stderr, check=False)
                captured = stdout.read_text(errors="replace")
                process = {**timing, "outputs": count, "files": files, "file_bytes": size, "mode": mode, "repetition": repetition, "stdout": captured, "stderr": stderr.read_text(errors="replace")}
                result["processes"].append(process)
                if timing["exit_code"] != 0:
                    raise common.BenchmarkError(f"filesystem-outputs probe failed ({timing['exit_code']}): {process['stderr'][-2000:]}")
                sample = parse_sample(captured, count, files, size, mode)
                result["samples"].append({**sample, "status": "ok", "implementation": "casita", "repetition": repetition, "wall_seconds": sample["nanos"] / 1e9, "max_rss_bytes": timing["max_rss_bytes"]})
                save(args, result)
        result["complete"] = True
    except Exception as error:
        result["error"] = str(error)
        save(args, result)
        raise
    save(args, result)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
