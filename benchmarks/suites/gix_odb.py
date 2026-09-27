#!/usr/bin/env python3
"""Gix object-database compatibility benchmarks for Casita."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import pathlib
import subprocess
import sys
import tempfile
from collections.abc import Sequence
from typing import Any

from benchmarks.suites import repository as common


SCHEMA_VERSION = 1
SAMPLE_PREFIX = "gix_odb_sample "
PROFILES = {
    "smoke": {
        "objects": 512,
        "body_bytes": 4096,
        "header_operations": 10_000,
        "miss_operations": 2_000,
    },
    "standard": {
        "objects": 8_192,
        "body_bytes": 4096,
        "header_operations": 100_000,
        "miss_operations": 20_000,
    },
    "huge": {
        "objects": 65_536,
        "body_bytes": 16 * 1024,
        "header_operations": 1_000_000,
        "miss_operations": 100_000,
    },
}

# Hold body size constant to expose the 8,192-entry metadata cache boundary.
PROFILES["below-cache"] = {**PROFILES["standard"], "objects": 4096}
PROFILES["cache-pressure"] = {**PROFILES["standard"], "objects": 16384}


class GixOdbBenchmarkError(RuntimeError):
    pass


def parse_benchmark_binary(output: str) -> pathlib.Path:
    executables = []
    for line in output.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        target = message.get("target", {})
        executable = message.get("executable")
        if (
            message.get("reason") == "compiler-artifact"
            and target.get("name") == "gix_odb"
            and "bench" in target.get("kind", [])
            and executable
        ):
            executables.append(pathlib.Path(executable))
    if len(executables) != 1:
        raise GixOdbBenchmarkError(
            f"cargo emitted {len(executables)} Gix ODB benchmark executables; expected one"
        )
    return executables[0]


def build_benchmark_binary() -> pathlib.Path:
    completed = subprocess.run(
        [
            "cargo",
            "bench",
            "--features",
            "git,experimental",
            "--bench",
            "gix_odb",
            "--no-run",
            "--message-format=json",
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode:
        raise GixOdbBenchmarkError(completed.stderr or completed.stdout)
    binary = parse_benchmark_binary(completed.stdout)
    if not binary.is_file():
        raise GixOdbBenchmarkError(f"benchmark executable does not exist: {binary}")
    return binary.resolve()


def parse_samples(output: str) -> list[dict[str, Any]]:
    samples = []
    for line in output.splitlines():
        if line.startswith(SAMPLE_PREFIX):
            sample = json.loads(line.removeprefix(SAMPLE_PREFIX))
            required = {
                "implementation",
                "backend",
                "operation",
                "operations",
                "wall_nanos",
                "nanos_per_op",
                "status",
            }
            missing = required - set(sample)
            if missing:
                raise GixOdbBenchmarkError(
                    f"Gix ODB sample is missing {sorted(missing)}"
                )
            samples.append(sample)
    if not samples:
        raise GixOdbBenchmarkError("Gix ODB benchmark emitted no samples")
    return samples


def run_repetition(
    binary: pathlib.Path,
    scale: dict[str, int],
    pack_cache_mib: int,
    repetition: int,
    workspace: pathlib.Path,
) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    environment = {
        **os.environ,
        "CASITA_GIX_ODB_OBJECTS": str(scale["objects"]),
        "CASITA_GIX_ODB_BODY_BYTES": str(scale["body_bytes"]),
        "CASITA_GIX_ODB_HEADER_OPERATIONS": str(scale["header_operations"]),
        "CASITA_GIX_ODB_MISS_OPERATIONS": str(scale["miss_operations"]),
        "CASITA_GIX_ODB_PACK_CACHE_BYTES": str(pack_cache_mib * 1024 * 1024),
    }
    stdout = workspace / f"repetition-{repetition:02d}.stdout"
    stderr = workspace / f"repetition-{repetition:02d}.stderr"
    command = common.CommandSpec([[str(binary)]], workspace, environment)
    process = common.measured_command(command, stdout, stderr)
    samples = parse_samples(stdout.read_text(errors="replace"))
    for sample in samples:
        sample["repetition"] = repetition
        sample["process_max_rss_bytes"] = process["max_rss_bytes"]
    return samples, {
        **process,
        "repetition": repetition,
        "command": command.display(),
        "stdout": common.captured_output(stdout),
        "stderr": common.captured_output(stderr),
    }


def render_report(result: dict[str, Any]) -> str:
    configuration = result["configuration"]
    lines = [
        "# Casita Gix ODB benchmark",
        "",
        (
            f"Profile: `{configuration['profile']}`; objects: "
            f"{configuration['scale']['objects']:,}; body bytes: "
            f"{configuration['scale']['body_bytes']:,}; repetitions: "
            f"{configuration['repetitions']}; pack cache: "
            f"{configuration.get('pack_cache_mib', 64)} MiB."
        ),
        "",
        "| Implementation | Backend | Operation | Repetition | ns/op | Throughput | Pack ranges | Pack hits |",
        "|---|---|---|---:|---:|---:|---:|---:|",
    ]
    for sample in result["samples"]:
        pack = sample.get("pack", {})
        throughput = sample.get("throughput_bytes_per_second", 0)
        lines.append(
            "| {implementation} | {backend} | {operation} | {repetition} | "
            "{nanos:.1f} | {throughput} | {ranges} | {hits} |".format(
                **sample,
                nanos=float(sample["nanos_per_op"]),
                throughput=(
                    f"{common.human_bytes(int(throughput))}/s" if throughput else "—"
                ),
                ranges=pack.get("chunk_range_requests", "—"),
                hits=pack.get("cache_hits", "—"),
            )
        )
    lines.extend(
        [
            "",
            "Gix memory and loose-object rows are compatibility baselines, not semantic equivalents to Casita's verified, deduplicated, transactionally published storage.",
            "Setup and full byte-for-byte validation are outside each timed operation. Casita write includes the required flush; reopened-header includes repository reopen setup only outside its timed lookup loop.",
            "",
        ]
    )
    return "\n".join(lines)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=sorted(PROFILES), default="smoke")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--pack-cache-mib", type=int, default=64)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--benchmark-bin", type=pathlib.Path)
    parser.add_argument("--keep-work", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--report", type=pathlib.Path)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    temporary: tempfile.TemporaryDirectory[str] | None = None
    try:
        if args.repetitions < 1:
            raise GixOdbBenchmarkError("repetitions must be positive")
        if args.pack_cache_mib < 0:
            raise GixOdbBenchmarkError("pack cache MiB must be non-negative")
        if args.no_build:
            if args.benchmark_bin is None:
                raise GixOdbBenchmarkError("--no-build requires --benchmark-bin")
            binary = args.benchmark_bin.resolve()
        else:
            print("building optimized Gix ODB benchmark", flush=True)
            binary = build_benchmark_binary()
        if not binary.is_file():
            raise GixOdbBenchmarkError(f"benchmark executable does not exist: {binary}")

        timestamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        args.output = args.output or pathlib.Path("benchmarks/results") / f"gix-odb-{timestamp}.json"
        args.report = args.report or args.output.with_suffix(".md")
        if args.keep_work:
            workspace = args.keep_work.resolve()
            workspace.mkdir(parents=True, exist_ok=True)
        else:
            temporary = tempfile.TemporaryDirectory(prefix="casita-gix-odb-")
            workspace = pathlib.Path(temporary.name)

        scale = PROFILES[args.profile]
        samples: list[dict[str, Any]] = []
        processes: list[dict[str, Any]] = []
        for repetition in range(1, args.repetitions + 1):
            print(
                f"[{repetition}/{args.repetitions}] {scale['objects']:,} objects x "
                f"{scale['body_bytes']:,} bytes",
                flush=True,
            )
            measured, process = run_repetition(
                binary, scale, args.pack_cache_mib, repetition, workspace
            )
            samples.extend(measured)
            processes.append(process)

        result = {
            "result_schema": "casita.gix-odb.v1",
            "suite_id": "native-git",
            "schema_version": SCHEMA_VERSION,
            "environment": common.environment_metadata(workspace),
            "configuration": {
                "profile": args.profile,
                "repetitions": args.repetitions,
                "pack_cache_mib": args.pack_cache_mib,
                "scale": scale,
                "argv": list(sys.argv if argv is None else [sys.argv[0], *argv]),
            },
            "tools": {"benchmark": str(binary)},
            "processes": processes,
            "samples": samples,
        }
        common.write_atomic(args.output, json.dumps(result, indent=2, sort_keys=True) + "\n")
        common.write_atomic(args.report, render_report(result))
        print(f"raw results: {args.output}")
        print(f"report: {args.report}")
        return 0
    except (GixOdbBenchmarkError, common.BenchmarkError, OSError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    finally:
        if temporary is not None:
            temporary.cleanup()


if __name__ == "__main__":
    raise SystemExit(main())
