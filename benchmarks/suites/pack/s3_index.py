#!/usr/bin/env python3
"""Benchmark persistent pack-index opens through the real S3/wal3 profile."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import pathlib
import statistics
import subprocess
import tempfile
import time
from collections.abc import Sequence
from typing import Any

from benchmarks.lib.budgets import entrypoint_budgets
from benchmarks.suites import repository as bench
from benchmarks.suites.pack import gc as pack_gc
from benchmarks.suites.pack.s3_gc import Rustfs


MIB = 1024 * 1024


def request_ledger(metrics: dict[str, int]) -> dict[str, dict[str, int]]:
    def wal_phase(prefix: str, *, include_writer_open: bool = False) -> dict[str, int]:
        requests = {
            "writer_manifest_get": (
                metrics[f"{prefix}_wal_writer_open_requests"] if include_writer_open else 0
            ),
            "manifest_get": metrics.get(f"{prefix}_wal_manifest_load_requests", 0),
            "conditional_manifest_get": metrics.get(
                f"{prefix}_wal_manifest_refresh_requests", 0
            ),
            "fragment_get": metrics[f"{prefix}_wal_fragment_get_requests"],
            "fragment_put": metrics.get(f"{prefix}_wal_fragment_put_requests", 0),
            "manifest_put": metrics.get(f"{prefix}_wal_manifest_put_requests", 0),
        }
        requests["total"] = sum(requests.values())
        return requests

    warm_open = {
        "payload_catalog_get": (
            metrics["warm_index_pointer_requests"] + metrics["warm_index_requests"]
        ),
        "payload_list": metrics["warm_list_requests"],
        "payload_footer_get": metrics["warm_footer_range_requests"],
        "payload_catalog_put": metrics["warm_index_put_requests"],
    }
    wal_open = wal_phase("warm_open", include_writer_open=True)
    warm_open.update({f"wal_{name}": value for name, value in wal_open.items() if name != "total"})
    warm_open["total"] = sum(warm_open.values())
    return {
        "warm_open": warm_open,
        "warm_first_snapshot": wal_phase("warm_first_snapshot"),
        "warm_repeat_snapshot": wal_phase("warm_repeat_snapshot"),
    }


def request_budget_checks(
    metrics: dict[str, int], budgets: dict[str, int] | None = None
) -> list[dict[str, object]]:
    budgets = budgets or entrypoint_budgets("s3-pack-index")
    ledger = request_ledger(metrics)
    sharded = bool(metrics["warm_index_sharded_base"])
    catalog_budget = (
        "warm_sharded_catalog_get_requests"
        if sharded
        else "warm_catalog_get_requests"
    )
    open_budget = (
        "warm_sharded_open_total_requests"
        if sharded
        else "warm_open_total_requests"
    )
    measured = {
        catalog_budget: metrics["warm_index_pointer_requests"]
        + metrics["warm_index_requests"],
        "warm_list_requests": metrics["warm_list_requests"],
        "warm_footer_range_requests": metrics["warm_footer_range_requests"],
        open_budget: ledger["warm_open"]["total"],
        "warm_first_snapshot_total_requests": ledger["warm_first_snapshot"]["total"],
        "warm_repeat_snapshot_total_requests": ledger["warm_repeat_snapshot"]["total"],
    }
    return [
        {
            "id": name.replace("_", "-"),
            "measured": measured[name],
            "limit": limit,
            "operator": "==",
            "unit": "requests",
            "status": "passed" if measured[name] == limit else "failed",
        }
        for name, limit in budgets.items()
        if name in measured
    ]


def enforce_request_budgets(metrics: dict[str, int]) -> None:
    failed = [check for check in request_budget_checks(metrics) if check["status"] == "failed"]
    if failed:
        details = ", ".join(
            f"{check['id']}={check['measured']} (required {check['limit']})" for check in failed
        )
        raise bench.BenchmarkError(f"S3 warm-open request budget failed: {details}")


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
        "pack_count",
        "cold_wall_nanos",
        "cold_command_nanos",
        "cold_first_snapshot_nanos",
        "cold_repeat_snapshot_nanos",
        "cold_payload_open_nanos",
        "cold_state_open_nanos",
        "cold_list_requests",
        "cold_footer_range_requests",
        "cold_footer_range_bytes",
        "cold_index_fallbacks",
        "cold_index_pointer_requests",
        "cold_index_put_requests",
        "cold_index_put_bytes",
        "cold_index_sharded_base",
        "cold_index_checkpoint_base",
        "cold_index_run_objects",
        "warm_wall_nanos",
        "warm_command_nanos",
        "warm_first_snapshot_nanos",
        "warm_repeat_snapshot_nanos",
        "warm_payload_open_nanos",
        "warm_state_open_nanos",
        "warm_list_requests",
        "warm_footer_range_requests",
        "warm_footer_range_bytes",
        "warm_index_pointer_requests",
        "warm_index_requests",
        "warm_index_bytes",
        "warm_index_hash_nanos",
        "warm_index_decode_nanos",
        "warm_index_hits",
        "warm_index_fallbacks",
        "warm_index_put_requests",
        "warm_index_sharded_base",
        "warm_index_checkpoint_base",
        "warm_index_run_objects",
        "warm_open_wal_writer_open_nanos",
        "warm_open_wal_writer_open_requests",
        "warm_open_wal_manifest_load_requests",
        "warm_open_wal_fragment_get_requests",
        "warm_open_wal_fragment_get_bytes",
        "warm_open_wal_fragment_records",
        "warm_open_wal_fragment_record_bytes",
        "warm_open_wal_fragment_get_nanos",
        "warm_open_wal_parquet_parse_nanos",
        "warm_open_wal_state_decode_nanos",
        "warm_open_wal_fragment_put_requests",
        "warm_open_wal_manifest_put_requests",
        "warm_open_wal_checkpoint_bytes",
        "warm_open_wal_checkpoint_objects",
        "warm_open_wal_checkpoint_roots",
        "warm_open_wal_tail_deltas",
        "warm_first_snapshot_wal_manifest_refresh_requests",
        "warm_first_snapshot_wal_manifest_refresh_nanos",
        "warm_first_snapshot_wal_manifest_load_requests",
        "warm_first_snapshot_wal_fragment_get_requests",
        "warm_first_snapshot_wal_cache_hits",
        "warm_first_snapshot_wal_cache_misses",
        "warm_first_snapshot_wal_fragment_put_requests",
        "warm_first_snapshot_wal_manifest_put_requests",
        "warm_repeat_snapshot_wal_manifest_refresh_requests",
        "warm_repeat_snapshot_wal_manifest_refresh_nanos",
        "warm_repeat_snapshot_wal_fragment_get_requests",
        "warm_repeat_snapshot_wal_cache_hits",
        "warm_repeat_snapshot_wal_fragment_put_requests",
        "warm_repeat_snapshot_wal_manifest_put_requests",
    }
    missing = required - metrics.keys()
    if missing:
        raise bench.BenchmarkError(f"RustFS helper omitted metrics: {sorted(missing)}")
    if metrics["cold_index_pointer_requests"] != 0:
        raise bench.BenchmarkError("cold WAL3 open unexpectedly read the advisory pointer")
    if metrics["cold_list_requests"] != 0 or metrics["cold_footer_range_requests"] != 0:
        raise bench.BenchmarkError("cold WAL3 open fell back to payload inventory")
    if metrics["cold_index_hits"] != 1 or metrics["cold_index_fallbacks"] != 0:
        raise bench.BenchmarkError("cold WAL3 open did not use its state catalog")
    if metrics["cold_index_put_requests"] != 0:
        raise bench.BenchmarkError("cold WAL3 open unexpectedly published a catalog")
    if metrics["cold_index_sharded_base"] != metrics["warm_index_sharded_base"]:
        raise bench.BenchmarkError("cold and warm opens disagreed on catalog base type")
    if metrics["cold_index_checkpoint_base"] != metrics["warm_index_checkpoint_base"]:
        raise bench.BenchmarkError("cold and warm opens disagreed on catalog base type")
    if metrics["cold_index_run_objects"] != metrics["warm_index_run_objects"]:
        raise bench.BenchmarkError("cold and warm opens disagreed on catalog run count")
    if metrics["warm_index_pointer_requests"] != 0:
        raise bench.BenchmarkError("warm WAL3 open unexpectedly read the advisory pointer")
    if metrics["warm_index_hits"] != 1 or metrics["warm_index_fallbacks"] != 0:
        raise bench.BenchmarkError("warm S3 open did not use its state catalog")
    enforce_request_budgets(metrics)
    if metrics["warm_index_put_requests"] != 0:
        raise bench.BenchmarkError("warm S3 open unexpectedly published a catalog")
    if metrics["warm_open_wal_manifest_load_requests"] != 0:
        raise bench.BenchmarkError("warm wal3 open did not reuse the writer manifest")
    if metrics["warm_open_wal_fragment_get_requests"] != 2:
        raise bench.BenchmarkError(
            "warm wal3 delta-tail open did not fetch exactly its tail and checkpoint"
        )
    if metrics["warm_open_wal_writer_open_requests"] != 1:
        raise bench.BenchmarkError("warm wal3 open did not fetch exactly one initial manifest")
    if metrics["warm_open_wal_fragment_put_requests"] != 0 or metrics[
        "warm_open_wal_manifest_put_requests"
    ] != 0:
        raise bench.BenchmarkError("warm wal3 open unexpectedly wrote state")
    for phase in ("warm_first_snapshot", "warm_repeat_snapshot"):
        if metrics[f"{phase}_wal_manifest_refresh_requests"] != 1:
            raise bench.BenchmarkError(f"{phase} did not issue exactly one manifest refresh")
        if metrics[f"{phase}_wal_fragment_get_requests"] != 0:
            raise bench.BenchmarkError(f"{phase} unexpectedly fetched a wal3 fragment")
        if metrics[f"{phase}_wal_fragment_put_requests"] != 0 or metrics[
            f"{phase}_wal_manifest_put_requests"
        ] != 0:
            raise bench.BenchmarkError(f"{phase} unexpectedly wrote wal3 state")
        if metrics[f"{phase}_wal_cache_hits"] != 1:
            raise bench.BenchmarkError(f"{phase} did not reuse the decoded checkpoint")
    if metrics["warm_first_snapshot_wal_manifest_load_requests"] != 0:
        raise bench.BenchmarkError("unchanged first snapshot reloaded the wal3 manifest")
    if metrics["warm_first_snapshot_wal_cache_misses"] != 0:
        raise bench.BenchmarkError("unchanged first snapshot missed the wal3 checkpoint cache")
    return metrics


def catalog_layout(samples: Sequence[dict[str, int]]) -> str:
    layouts = set()
    for sample in samples:
        if sample["warm_index_sharded_base"]:
            base = "sharded"
        elif sample["warm_index_checkpoint_base"]:
            base = "checkpoint"
        else:
            base = "inline"
        runs = sample["warm_index_run_objects"]
        layouts.add(f"{base}+{runs} run{'s' if runs != 1 else ''}" if runs else base)
    return layouts.pop() if len(layouts) == 1 else "mixed"


def render_report(result: dict[str, object]) -> str:
    grouped: dict[tuple[int, int], list[dict[str, int]]] = {}
    process_grouped: dict[tuple[int, int], list[dict[str, object]]] = {}
    for sample in result["samples"]:
        key = (int(sample["files"]), int(sample["target_mib"]))
        grouped.setdefault(key, []).append(sample["metrics"])
        process_grouped.setdefault(key, []).append(sample)
    lines = [
        "# Casita RustFS/wal3 persistent pack-index benchmark",
        "",
        "Each sample imports into a fresh RustFS bucket through wal3, deletes the advisory payload pointer, then measures cold and warm state-seeded repository opens. It also times the first complete state snapshot and one unchanged cached snapshot.",
        "",
        "| Files | Target | n | Packs | Base | Cold open | Warm open | Open speedup | Warm open+snapshot | First snapshot | Cached snapshot | Warm payload | Warm wal3 | Cold footer GETs | Warm footer GETs | Footer bytes | Catalog bytes | Hash | Decode |",
        "|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for (files, target), samples in sorted(grouped.items()):
        cold = [sample["cold_wall_nanos"] / 1_000_000_000 for sample in samples]
        warm = [sample["warm_wall_nanos"] / 1_000_000_000 for sample in samples]
        cold_median = statistics.median(cold)
        warm_median = statistics.median(warm)
        speedup = cold_median / warm_median if warm_median else float("inf")

        def median(name: str) -> float:
            return statistics.median(sample[name] for sample in samples)

        warm_payload = median("warm_payload_open_nanos") / 1_000_000_000
        warm_state = median("warm_state_open_nanos") / 1_000_000_000
        warm_command = median("warm_command_nanos") / 1_000_000_000
        warm_first_snapshot = median("warm_first_snapshot_nanos") / 1_000_000_000
        warm_repeat_snapshot = median("warm_repeat_snapshot_nanos") / 1_000_000_000
        lines.append(
            f"| {files} | {target} MiB | {len(samples)} | {median('pack_count'):g} | "
            f"{catalog_layout(samples)} | "
            f"{cold_median:.4f} s | {warm_median:.4f} s | {speedup:.2f}x | "
            f"{warm_command:.4f} s | {warm_first_snapshot:.4f} s | "
            f"{warm_repeat_snapshot:.4f} s | "
            f"{warm_payload:.4f} s | {warm_state:.4f} s | "
            f"{median('cold_footer_range_requests'):g} | "
            f"{median('warm_footer_range_requests'):g} | "
            f"{bench.human_bytes(int(median('cold_footer_range_bytes')))} | "
            f"{bench.human_bytes(int(median('warm_index_bytes')))} | "
            f"{median('warm_index_hash_nanos') / 1_000_000:.3f} ms | "
            f"{median('warm_index_decode_nanos') / 1_000_000:.3f} ms |"
        )
    lines.extend(
        [
            "",
            "## Helper process resources",
            "",
            "This includes fixture import plus cold and warm opens; the narrower open timings above come from inside the helper.",
            "",
            "| Files | Target | Median process wall | Median peak RSS |",
            "|---:|---:|---:|---:|",
        ]
    )
    for (files, target), samples in sorted(process_grouped.items()):
        lines.append(
            f"| {files} | {target} MiB | "
            f"{statistics.median(float(sample.get('process_wall_seconds', 0)) for sample in samples):.4f} s | "
            f"{bench.human_bytes(int(statistics.median(float(sample.get('max_rss_bytes', 0)) for sample in samples)))} |"
        )
    lines.extend(
        [
            "",
            "Both reopens recover the v1 root from WAL3 before opening payloads, so deleting the advisory pointer causes no pointer GET, inventory LIST, or footer GET. Referenced immutable runs, when present, remain ordinary catalog GETs. The first and cached snapshot columns include WAL3's conditional manifest refresh.",
            "",
            "## Warm wal3 phase breakdown",
            "",
            "Writer open includes wal3's initial manifest GET. Fragment GET is transfer only; Parquet and state decode are measured separately.",
            "",
            "| Files | Target | Objects | State bytes | Tail deltas | Fragment records | Record bytes | wal3 open | Writer/manifest | Fragment GET | Fragment bytes | Parquet | State decode | First refresh | Cached refresh |",
            "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for (files, target), samples in sorted(grouped.items()):
        def median(name: str) -> float:
            return statistics.median(sample[name] for sample in samples)

        lines.append(
            f"| {files} | {target} MiB | {median('warm_open_wal_checkpoint_objects'):g} | "
            f"{bench.human_bytes(int(median('warm_open_wal_checkpoint_bytes')))} | "
            f"{median('warm_open_wal_tail_deltas'):g} | "
            f"{median('warm_open_wal_fragment_records'):g} | "
            f"{bench.human_bytes(int(median('warm_open_wal_fragment_record_bytes')))} | "
            f"{median('warm_state_open_nanos') / 1_000_000:.3f} ms | "
            f"{median('warm_open_wal_writer_open_nanos') / 1_000_000:.3f} ms | "
            f"{median('warm_open_wal_fragment_get_nanos') / 1_000_000:.3f} ms | "
            f"{bench.human_bytes(int(median('warm_open_wal_fragment_get_bytes')))} | "
            f"{median('warm_open_wal_parquet_parse_nanos') / 1_000_000:.3f} ms | "
            f"{median('warm_open_wal_state_decode_nanos') / 1_000_000:.3f} ms | "
            f"{median('warm_first_snapshot_wal_manifest_refresh_nanos') / 1_000_000:.3f} ms | "
            f"{median('warm_repeat_snapshot_wal_manifest_refresh_nanos') / 1_000_000:.3f} ms |"
        )
    lines.append("")
    lines.extend(
        [
            "## Complete warm request ledger",
            "",
            "Counts include the payload catalog and wal3. Writer open's initial manifest GET is explicit; each unchanged snapshot uses one conditional manifest GET.",
            "",
            "| Files | Target | Warm open | Payload | wal3 manifest | wal3 fragment | First snapshot | Cached snapshot |",
            "|---:|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for (files, target), samples in sorted(grouped.items()):
        ledgers = [request_ledger(sample) for sample in samples]

        def ledger_median(phase: str, name: str) -> float:
            return statistics.median(ledger[phase][name] for ledger in ledgers)

        payload = sum(
            ledger_median("warm_open", name)
            for name in (
                "payload_catalog_get",
                "payload_list",
                "payload_footer_get",
                "payload_catalog_put",
            )
        )
        wal_manifest = sum(
            ledger_median("warm_open", name)
            for name in (
                "wal_writer_manifest_get",
                "wal_manifest_get",
                "wal_conditional_manifest_get",
                "wal_manifest_put",
            )
        )
        wal_fragment = sum(
            ledger_median("warm_open", name)
            for name in ("wal_fragment_get", "wal_fragment_put")
        )
        lines.append(
            f"| {files} | {target} MiB | {ledger_median('warm_open', 'total'):g} | "
            f"{payload:g} | {wal_manifest:g} | {wal_fragment:g} | "
            f"{ledger_median('warm_first_snapshot', 'total'):g} | "
            f"{ledger_median('warm_repeat_snapshot', 'total'):g} |"
        )
    lines.append("")
    if result.get("budgets"):
        checks = [
            check
            for sample in result["samples"]
            for check in sample.get("budget_checks", [])
        ]
        status = result["budget_summary"]["status"]
        lines.extend(
            [
                "## Request budgets",
                "",
                f"Overall status: **{status}**.",
                "",
                "| Check | Required | Observed samples | Status |",
                "|---|---:|---:|---|",
            ]
        )
        for name, limit in result["budgets"].items():
            identifier = name.replace("_", "-")
            matching = [check for check in checks if check["id"] == identifier]
            observed = ", ".join(str(check["measured"]) for check in matching)
            check_status = (
                "passed"
                if matching and all(check["status"] == "passed" for check in matching)
                else "not applicable"
                if not matching
                else "failed"
            )
            lines.append(f"| {identifier} | {limit} | {observed or '—'} | {check_status} |")
        lines.append("")
    return "\n".join(lines)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--targets-mib", default="16,64,128,256")
    parser.add_argument("--files", default="8192", help="comma-separated state cardinalities")
    parser.add_argument("--file-kib", type=int, default=8)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument(
        "--helper", type=pathlib.Path, default=pathlib.Path("target/release/examples/pack_index_rustfs")
    )
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--keep-work", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--report", type=pathlib.Path)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    targets = pack_gc.unique_positive_csv(args.targets_mib)
    file_counts = pack_gc.unique_positive_csv(args.files)
    if min(args.file_kib, args.repetitions) < 1:
        raise SystemExit("--file-kib and --repetitions must be positive")
    budgets = entrypoint_budgets("s3-pack-index")
    if not args.no_build:
        subprocess.run(
            ["cargo", "build", "--release", "--example", "pack_index_rustfs", "--features", "s3,experimental"],
            check=True,
        )
    args.helper = args.helper.resolve()
    if not args.helper.exists():
        raise SystemExit(f"RustFS benchmark helper does not exist: {args.helper}")

    temporary = None
    if args.keep_work is None:
        temporary = tempfile.TemporaryDirectory(prefix="casita-s3-pack-index-")
        work = pathlib.Path(temporary.name)
    else:
        work = args.keep_work.resolve()
        work.mkdir(parents=True, exist_ok=True)
    timestamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    output = args.output or pathlib.Path("benchmarks/results") / f"s3-pack-index-{timestamp}.json"
    report = args.report or output.with_suffix(".md")
    sources: dict[int, pathlib.Path] = {}
    for files in file_counts:
        source = work / f"source-{files}"
        pack_gc.generate_files(source, files, args.file_kib * 1024)
        sources[files] = source
    rustfs = Rustfs(work / "rustfs")
    samples: list[dict[str, object]] = []
    try:
        total = len(file_counts) * len(targets) * args.repetitions
        index = 0
        for files in file_counts:
            for target in targets:
                for repetition in range(1, args.repetitions + 1):
                    index += 1
                    print(
                        f"[{index}/{total}] files={files} target={target}MiB "
                        f"repetition={repetition}",
                        flush=True,
                    )
                    writer = f"bench-{files}-{target}-{repetition}-{time.time_ns()}"
                    helper_command = [
                            str(args.helper),
                            str(sources[files]),
                            str(target * MIB),
                            writer,
                            rustfs.endpoint,
                    ]
                    stdout = work / f"helper-{files}-{target}-{repetition}.stdout"
                    stderr = work / f"helper-{files}-{target}-{repetition}.stderr"
                    process_metrics = bench.measured_command(
                        bench.CommandSpec([helper_command], work, dict(os.environ)),
                        stdout,
                        stderr,
                    )
                    metrics = parse_metrics(stdout.read_text(errors="replace"))
                    samples.append(
                        {
                            "files": files,
                            "target_mib": target,
                            "repetition": repetition,
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
            "result_schema": "casita.s3-pack-index.v5",
            "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "configuration": {
                "endpoint": rustfs.endpoint,
                "targets_mib": targets,
                "files": file_counts,
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
