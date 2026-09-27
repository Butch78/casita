#!/usr/bin/env python3
"""Run the pack-GC density matrix against a local RustFS S3 endpoint.

The Rust helper uses Casita's real S3 payload store and wal3 logical state.
Every sample receives a fresh bucket, and RustFS data lives only for this run.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import pathlib
import shutil
import socket
import statistics
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from collections.abc import Sequence
from typing import Any

from benchmarks.lib.budgets import entrypoint_budgets
from benchmarks.suites import repository as bench
from benchmarks.suites.pack import gc as pack_gc


MIB = 1024 * 1024


def request_ledger(metrics: dict[str, int]) -> dict[str, int]:
    ledger = {
        "payload_list": (
            metrics["pack_list_requests"]
            + metrics["pack_gc_manifest_list_requests"]
            + metrics["pack_gc_loose_chunk_list_requests"]
        ),
        "payload_get": (
            metrics["pack_footer_range_requests"]
            + metrics["pack_chunk_range_requests"]
            + metrics["pack_whole_requests"]
            + metrics["pack_index_pointer_requests"]
            + metrics["pack_index_requests"]
        ),
        "payload_put": (
            metrics["pack_gc_replacement_put_requests"]
            + metrics["pack_gc_marker_put_requests"]
            + metrics["pack_gc_tombstone_put_requests"]
            + metrics["pack_index_put_requests"]
        ),
        "payload_delete": (
            metrics["pack_gc_delete_requests"]
            + metrics["pack_gc_manifest_delete_requests"]
            + metrics["pack_gc_outboard_delete_requests"]
            + metrics["pack_gc_loose_chunk_delete_requests"]
            + metrics["pack_gc_tombstone_delete_requests"]
        ),
        "wal_get": (
            metrics["gc_wal_writer_open_requests"]
            + metrics["gc_wal_manifest_load_requests"]
            + metrics["gc_wal_manifest_refresh_requests"]
            + metrics["gc_wal_fragment_get_requests"]
            + metrics["gc_wal_logical_shard_get_requests"]
            + metrics["gc_wal_logical_shard_barrier_get_requests"]
        ),
        "wal_put": (
            metrics["gc_wal_fragment_put_requests"]
            + metrics["gc_wal_manifest_put_requests"]
            + metrics["gc_wal_logical_shard_put_requests"]
            + metrics["gc_wal_logical_shard_barrier_put_requests"]
        ),
        "wal_list": metrics["gc_wal_logical_shard_inventory_list_requests"],
        "wal_delete": metrics["gc_wal_logical_shard_delete_requests"],
    }
    ledger["total"] = sum(ledger.values())
    return ledger


def request_budget_checks(
    metrics: dict[str, int], budgets: dict[str, int] | None = None
) -> list[dict[str, object]]:
    budgets = budgets or entrypoint_budgets("s3-pack-gc")
    ledger = request_ledger(metrics)
    measured = {
        "gc_wal_requests": (
            ledger["wal_get"] + ledger["wal_put"] + ledger["wal_list"] + ledger["wal_delete"]
        ),
        "gc_inventory_list_requests": (
            metrics["pack_gc_manifest_list_requests"]
            + metrics["pack_gc_loose_chunk_list_requests"]
        ),
        "gc_catalog_put_requests": metrics["pack_index_put_requests"],
        "gc_survivor_range_requests": metrics["pack_chunk_range_requests"],
        "gc_unexpected_catalog_read_requests": (
            metrics["pack_list_requests"]
            + metrics["pack_footer_range_requests"]
            + metrics["pack_index_pointer_requests"]
            + metrics["pack_index_requests"]
        ),
        "max_gc_tombstone_put_requests": metrics["pack_gc_tombstone_put_requests"],
        "gc_logical_shard_barrier_get_requests": metrics[
            "gc_wal_logical_shard_barrier_get_requests"
        ],
        "gc_logical_shard_barrier_put_requests": metrics[
            "gc_wal_logical_shard_barrier_put_requests"
        ],
        "gc_logical_shard_inventory_list_requests": metrics[
            "gc_wal_logical_shard_inventory_list_requests"
        ],
        "max_gc_logical_shard_put_requests": metrics[
            "gc_wal_logical_shard_put_requests"
        ],
    }
    checks = []
    for name, limit in budgets.items():
        operator = "<=" if name.startswith("max_") else "=="
        passed = measured[name] <= limit if operator == "<=" else measured[name] == limit
        checks.append(
            {
                "id": name.replace("_", "-"),
                "measured": measured[name],
                "limit": limit,
                "operator": operator,
                "unit": "requests",
                "status": "passed" if passed else "failed",
            }
        )
    return checks


def enforce_request_budgets(metrics: dict[str, int]) -> None:
    failed = [check for check in request_budget_checks(metrics) if check["status"] == "failed"]
    if failed:
        details = ", ".join(
            f"{check['id']}={check['measured']} ({check['operator']} {check['limit']})"
            for check in failed
        )
        raise bench.BenchmarkError(f"S3 GC request budget failed: {details}")


def summarize_request_budgets(samples: Sequence[dict[str, Any]]) -> dict[str, object]:
    checks = [check for sample in samples for check in sample.get("budget_checks", [])]
    failed = sum(check["status"] == "failed" for check in checks)
    passed = sum(check["status"] == "passed" for check in checks)
    return {
        "status": "failed" if failed else "passed" if passed else "not-applicable",
        "passed": passed,
        "failed": failed,
    }


def parse_metrics(stdout: str) -> dict[str, int]:
    metrics: dict[str, int] = {}
    for line in stdout.splitlines():
        parts = line.split()
        if len(parts) != 2:
            continue
        try:
            metrics[parts[0].replace("-", "_")] = int(parts[1])
        except ValueError:
            continue
    required = {
        "gc_wall_nanos",
        "removed_payloads",
        "removed_chunks",
        "pack_list_requests",
        "pack_gc_manifest_list_requests",
        "pack_gc_loose_chunk_list_requests",
        "pack_footer_range_requests",
        "pack_chunk_range_requests",
        "pack_whole_requests",
        "pack_whole_bytes",
        "pack_gc_replacement_put_requests",
        "pack_gc_replacement_put_bytes",
        "pack_gc_marker_put_requests",
        "pack_gc_delete_requests",
        "pack_gc_manifest_delete_requests",
        "pack_gc_outboard_delete_requests",
        "pack_gc_loose_chunk_delete_requests",
        "pack_gc_tombstone_put_requests",
        "pack_gc_tombstone_put_bytes",
        "pack_gc_tombstone_delete_requests",
        "pack_gc_deferred_packs",
        "pack_index_pointer_requests",
        "pack_index_requests",
        "pack_index_put_requests",
        "gc_wal_writer_open_requests",
        "gc_wal_manifest_load_requests",
        "gc_wal_manifest_refresh_requests",
        "gc_wal_fragment_get_requests",
        "gc_wal_fragment_put_requests",
        "gc_wal_manifest_put_requests",
        "gc_wal_logical_shard_get_requests",
        "gc_wal_logical_shard_put_requests",
        "gc_wal_logical_shard_barrier_get_requests",
        "gc_wal_logical_shard_barrier_put_requests",
        "gc_wal_logical_shard_inventory_list_requests",
        "gc_wal_logical_shard_delete_requests",
    }
    missing = required - metrics.keys()
    if missing:
        raise bench.BenchmarkError(f"RustFS helper omitted metrics: {sorted(missing)}")
    if metrics["pack_chunk_range_requests"] != 0:
        raise bench.BenchmarkError("RustFS GC used per-survivor range reads")
    if metrics["pack_whole_requests"] != metrics["pack_gc_replacement_put_requests"]:
        raise bench.BenchmarkError("RustFS GC did not pair each whole GET with one replacement PUT")
    if metrics["pack_gc_marker_put_requests"] != metrics["pack_gc_delete_requests"]:
        raise bench.BenchmarkError("RustFS GC marker PUT and old-pack DELETE counts differ")
    expected_delta_puts = int(metrics["pack_gc_deferred_packs"] > 0)
    if metrics["pack_gc_tombstone_put_requests"] != expected_delta_puts:
        raise bench.BenchmarkError("RustFS GC did not batch deferred packs into one delta PUT")
    enforce_request_budgets(metrics)
    return metrics


def render_report(result: dict[str, object]) -> str:
    grouped: dict[tuple[int, int], list[dict[str, object]]] = {}
    for sample in result["samples"]:
        grouped.setdefault(
            (int(sample["target_mib"]), int(sample["requested_dead_percent"])), []
        ).append(sample)
    lines = [
        "# Casita RustFS pack GC density sweep",
        "",
        "Times cover GC only; bucket creation and the two imports are outside the timer.",
        "",
        "| Target | Requested dead | n | Median | p95 | Peak RSS | Whole GETs | Read | Replacement PUTs | Pack write | Deferred | Delta PUTs | Tombstone write | Deletes | Removed chunks |",
        "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for (target, density), samples in sorted(grouped.items()):
        walls = [float(sample["wall_seconds"]) for sample in samples]

        def median(name: str) -> float:
            return statistics.median(float(sample["metrics"][name]) for sample in samples)

        lines.append(
            f"| {target} MiB | {density}% | {len(samples)} | "
            f"{statistics.median(walls):.4f} s | {bench.percentile(walls, 0.95):.4f} s | "
            f"{bench.human_bytes(int(statistics.median(float(sample.get('max_rss_bytes', 0)) for sample in samples)))} | "
            f"{median('pack_whole_requests'):g} | "
            f"{bench.human_bytes(int(median('pack_whole_bytes')))} | "
            f"{median('pack_gc_replacement_put_requests'):g} | "
            f"{bench.human_bytes(int(median('pack_gc_replacement_put_bytes')))} | "
            f"{median('pack_gc_deferred_packs'):g} | "
            f"{median('pack_gc_tombstone_put_requests'):g} | "
            f"{bench.human_bytes(int(median('pack_gc_tombstone_put_bytes')))} | "
            f"{median('pack_gc_delete_requests'):g} | "
            f"{median('removed_chunks'):g} |"
        )
    lines.extend(
        [
            "",
            "Every sample uses the real S3 DeleteObjects/GET/PUT paths and wal3 state, performs zero survivor range reads, passes `fsck`, and verifies the retained closure.",
            "",
        ]
    )
    lines.extend(
        [
            "## Complete GC request ledger",
            "",
            "The total includes payload inventory, reads, writes, deletes, the catalog publication, and wal3's state refresh, reload, and durable append.",
            "",
            "| Target | Requested dead | Total | Payload LIST | Payload GET | Payload PUT | Payload DELETE | wal3 GET | wal3 PUT | wal3 LIST | wal3 DELETE |",
            "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for (target, density), samples in sorted(grouped.items()):
        ledgers = [request_ledger(sample["metrics"]) for sample in samples]

        def median_request(name: str) -> float:
            return statistics.median(ledger[name] for ledger in ledgers)

        lines.append(
            f"| {target} MiB | {density}% | {median_request('total'):g} | "
            f"{median_request('payload_list'):g} | {median_request('payload_get'):g} | "
            f"{median_request('payload_put'):g} | {median_request('payload_delete'):g} | "
            f"{median_request('wal_get'):g} | {median_request('wal_put'):g} | "
            f"{median_request('wal_list'):g} | {median_request('wal_delete'):g} |"
        )
    lines.append("")
    if result.get("budgets"):
        checks = [
            check for sample in result["samples"] for check in sample.get("budget_checks", [])
        ]
        lines.extend(
            [
                "## Request budgets",
                "",
                f"Overall status: **{result['budget_summary']['status']}**.",
                "",
                "| Check | Required | Observed samples | Status |",
                "|---|---:|---:|---|",
            ]
        )
        for name, limit in result["budgets"].items():
            identifier = name.replace("_", "-")
            matching = [check for check in checks if check["id"] == identifier]
            observed = ", ".join(str(check["measured"]) for check in matching)
            operator = matching[0]["operator"] if matching else "=="
            status = (
                "passed"
                if matching and all(check["status"] == "passed" for check in matching)
                else "failed"
            )
            lines.append(f"| {identifier} | {operator} {limit} | {observed} | {status} |")
        lines.append("")
    return "\n".join(lines)


class Rustfs:
    def __init__(
        self,
        root: pathlib.Path,
        address_port: int | None = None,
        console_port: int | None = None,
        reuse: bool = False,
    ):
        executable = shutil.which("rustfs")
        if executable is None:
            raise bench.BenchmarkError("rustfs is not available; enter the pinned devenv")
        root.mkdir(parents=True, exist_ok=reuse)
        if address_port is None:
            with socket.socket() as listener:
                listener.bind(("127.0.0.1", 0))
                address_port = listener.getsockname()[1]
        if console_port is None:
            with socket.socket() as listener:
                listener.bind(("127.0.0.1", 0))
                console_port = listener.getsockname()[1]
        self.port = address_port
        self.endpoint = f"http://127.0.0.1:{self.port}"
        self.log_path = root.parent / (root.name + ".log")
        with self.log_path.open("wb") as log:
            self.process = subprocess.Popen(
                [
                    executable,
                    "server",
                    str(root),
                    "--address",
                    f"127.0.0.1:{self.port}",
                    "--console-address",
                    f"127.0.0.1:{console_port}",
                ],
                env={**os.environ, "RUSTFS_ACCESS_KEY": "minio", "RUSTFS_SECRET_KEY": "minio123"},
                stdout=log,
                stderr=subprocess.STDOUT,
            )
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise bench.BenchmarkError(self.startup_error("rustfs exited before accepting connections"))
            try:
                with urllib.request.urlopen(
                    f"{self.endpoint}/minio/health/ready", timeout=1
                ) as response:
                    if response.status == 200:
                        return
            except (OSError, urllib.error.URLError):
                time.sleep(0.025)
        self.close()
        raise bench.BenchmarkError(self.startup_error("rustfs did not become ready within 60 seconds"))

    def startup_error(self, message: str) -> str:
        with self.log_path.open("rb") as log:
            log.seek(max(0, self.log_path.stat().st_size - 4096))
            tail = log.read().decode(errors="replace").strip()
        return f"{message}\n{tail}" if tail else message

    def close(self) -> None:
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--targets-mib", default="4,16")
    parser.add_argument("--dead-percent", default="1,10,50,100")
    parser.add_argument("--files", type=int, default=512)
    parser.add_argument("--file-kib", type=int, default=64)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument(
        "--helper", type=pathlib.Path, default=pathlib.Path("target/release/examples/pack_gc_rustfs")
    )
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--keep-work", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--report", type=pathlib.Path)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    targets = pack_gc.unique_positive_csv(args.targets_mib)
    densities = pack_gc.unique_positive_csv(args.dead_percent, maximum=100)
    if args.files < 1 or args.file_kib < 1 or args.repetitions < 1:
        raise SystemExit("--files, --file-kib, and --repetitions must be positive")
    budgets = entrypoint_budgets("s3-pack-gc")
    if not args.no_build:
        subprocess.run(
            [
                "cargo",
                "build",
                "--release",
                "--example",
                "pack_gc_rustfs",
                "--features",
                "s3,experimental",
            ],
            check=True,
        )
    args.helper = args.helper.resolve()
    if not args.helper.exists():
        raise SystemExit(f"RustFS benchmark helper does not exist: {args.helper}")

    temporary = None
    if args.keep_work is None:
        temporary = tempfile.TemporaryDirectory(prefix="casita-s3-pack-gc-")
        work = pathlib.Path(temporary.name)
    else:
        work = args.keep_work.resolve()
        work.mkdir(parents=True, exist_ok=True)
    timestamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    output = args.output or pathlib.Path("benchmarks/results") / f"s3-pack-gc-{timestamp}.json"
    report = args.report or output.with_suffix(".md")
    rustfs = Rustfs(work / "rustfs")
    samples: list[dict[str, object]] = []
    try:
        total = len(targets) * len(densities) * args.repetitions
        index = 0
        for target in targets:
            for density in densities:
                for repetition in range(1, args.repetitions + 1):
                    index += 1
                    print(
                        f"[{index}/{total}] target={target}MiB dead={density}% repetition={repetition}",
                        flush=True,
                    )
                    workspace = work / f"target-{target}-dead-{density}-r{repetition}"
                    base = workspace / "base"
                    retained = workspace / "retained"
                    pack_gc.generate_files(base, args.files, args.file_kib * 1024)
                    shutil.copytree(base, retained, copy_function=os.link)
                    removed = pack_gc.evenly_distributed_indexes(args.files, density)
                    for file_index in removed:
                        (retained / f"file-{file_index:06d}.bin").unlink()
                    writer = f"bench-{target}-{density}-{repetition}-{time.time_ns()}"
                    helper_command = [
                            str(args.helper),
                            str(base),
                            str(retained),
                            str(target * MIB),
                            writer,
                            rustfs.endpoint,
                    ]
                    stdout = workspace / "helper.stdout"
                    stderr = workspace / "helper.stderr"
                    process_metrics = bench.measured_command(
                        bench.CommandSpec([helper_command], workspace, dict(os.environ)),
                        stdout,
                        stderr,
                    )
                    metrics = parse_metrics(stdout.read_text(errors="replace"))
                    samples.append(
                        {
                            "target_mib": target,
                            "requested_dead_percent": density,
                            "repetition": repetition,
                            "files": args.files,
                            "removed_files": len(removed),
                            "file_bytes": args.file_kib * 1024,
                            "wall_seconds": metrics["gc_wall_nanos"] / 1_000_000_000,
                            "process_wall_seconds": process_metrics["wall_seconds"],
                            "process_user_seconds": process_metrics["user_seconds"],
                            "process_system_seconds": process_metrics["system_seconds"],
                            "max_rss_bytes": process_metrics["max_rss_bytes"],
                            "metrics": metrics,
                            "request_ledger": request_ledger(metrics),
                            "budget_checks": request_budget_checks(metrics, budgets),
                        }
                    )
        result: dict[str, object] = {
            "result_schema": "casita.s3-pack-gc.v2",
            "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "configuration": {
                "endpoint": rustfs.endpoint,
                "targets_mib": targets,
                "dead_percent": densities,
                "files": args.files,
                "file_kib": args.file_kib,
                "repetitions": args.repetitions,
            },
            "budgets": budgets,
            "budget_summary": summarize_request_budgets(samples),
            "samples": samples,
        }
        bench.write_atomic(output, json.dumps(result, indent=2, sort_keys=True) + "\n")
        bench.write_atomic(report, render_report(result))
        print(f"result: {output}")
        print(f"report: {report}")
    finally:
        rustfs.close()
        if temporary is not None:
            temporary.cleanup()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
