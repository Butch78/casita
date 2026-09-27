#!/usr/bin/env python3
"""Benchmark cold pack-index rebuilds against persistent-catalog opens."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import pathlib
import statistics
import subprocess
import tempfile
from collections.abc import Sequence

from benchmarks.suites import repository as bench


KIB = 1024
MIB = 1024 * KIB


def render_report(result: dict[str, object]) -> str:
    cold = result["cold"]
    warm = result["warm"]
    warm_walls = [float(sample["wall_seconds"]) for sample in warm]
    warm_rss = [float(sample["max_rss_bytes"]) for sample in warm]
    lines = [
        "# Casita persistent pack-index benchmark",
        "",
        "The cold sample opens with the catalog deliberately absent. It rebuilds from pack footers and publishes one checksummed catalog object. Each warm sample is a new process opening the unchanged repository.",
        "",
        "| Phase | n | Median wall | p95 wall | Median RSS | Footer GETs | Footer bytes | Index hits | Index bytes read | Hash | Decode | Index bytes written |",
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
        "| Cold rebuild | 1 | {wall:.4f} s | {wall:.4f} s | {rss} | {footer_gets} | {footer_bytes} | {hits} | {index_read} | {hash_ms:.3f} ms | {decode_ms:.3f} ms | {index_write} |".format(
            wall=float(cold["wall_seconds"]),
            rss=bench.human_bytes(int(cold["max_rss_bytes"])),
            footer_gets=cold["metrics"].get("pack_footer_range_requests", 0),
            footer_bytes=bench.human_bytes(cold["metrics"].get("pack_footer_range_bytes", 0)),
            hits=cold["metrics"].get("pack_index_hits", 0),
            index_read=bench.human_bytes(cold["metrics"].get("pack_index_bytes", 0)),
            hash_ms=cold["metrics"].get("pack_index_hash_nanos", 0) / 1_000_000,
            decode_ms=cold["metrics"].get("pack_index_decode_nanos", 0) / 1_000_000,
            index_write=bench.human_bytes(cold["metrics"].get("pack_index_put_bytes", 0)),
        ),
        "| Warm catalog | {count} | {wall:.4f} s | {p95:.4f} s | {rss} | 0 | 0 B | 1/open | {index_read} | {hash_ms:.3f} ms | {decode_ms:.3f} ms | 0 B |".format(
            count=len(warm),
            wall=statistics.median(warm_walls),
            p95=bench.percentile(warm_walls, 0.95),
            rss=bench.human_bytes(int(statistics.median(warm_rss))),
            index_read=bench.human_bytes(
                int(statistics.median(sample["metrics"]["pack_index_bytes"] for sample in warm))
            ),
            hash_ms=statistics.median(
                sample["metrics"]["pack_index_hash_nanos"] for sample in warm
            )
            / 1_000_000,
            decode_ms=statistics.median(
                sample["metrics"]["pack_index_decode_nanos"] for sample in warm
            )
            / 1_000_000,
        ),
        "",
        "Cold recovery performs four inventory LIST operations. A warm authoritative catalog replaces them and all per-pack footer and GC-record reads with one object GET.",
        "",
    ]
    return "\n".join(lines)


def measure(
    command: list[str], workspace: pathlib.Path, env: dict[str, str], label: str
) -> dict[str, object]:
    stdout = workspace / f"{label}.stdout"
    stderr = workspace / f"{label}.stderr"
    timing = bench.measured_command(
        bench.CommandSpec([command], workspace, env), stdout, stderr
    )
    if timing["exit_code"] != 0:
        raise bench.BenchmarkError(
            f"{label} failed ({timing['exit_code']}):\n{stderr.read_text(errors='replace')}"
        )
    metrics = bench.CasitaAdapter(command[0]).operation_metrics(
        "root-ls", stdout.read_text(errors="replace")
    )
    return {**timing, "metrics": metrics}


def validate(cold: dict[str, object], warm: list[dict[str, object]], packs: int) -> None:
    cold_metrics = cold["metrics"]
    if cold_metrics.get("pack_index_fallbacks") != 1:
        raise bench.BenchmarkError("cold open did not take exactly one index fallback")
    if cold_metrics.get("pack_index_put_requests") != 1:
        raise bench.BenchmarkError("cold open did not publish exactly one index catalog")
    if cold_metrics.get("pack_index_pointer_requests") != 1:
        raise bench.BenchmarkError("cold open did not probe the catalog exactly once")
    if cold_metrics.get("pack_list_requests") != 4:
        raise bench.BenchmarkError("cold open did not scan the four inventory prefixes")
    if cold_metrics.get("pack_footer_range_requests") != 2 * packs:
        raise bench.BenchmarkError("cold open did not read exactly two footer ranges per pack")
    for sample in warm:
        metrics = sample["metrics"]
        if metrics.get("pack_index_hits") != 1 or metrics.get("pack_index_fallbacks") != 0:
            raise bench.BenchmarkError("warm open did not use exactly one index catalog")
        if metrics.get("pack_footer_range_requests") != 0:
            raise bench.BenchmarkError("warm catalog open unexpectedly read a pack footer")
        if metrics.get("pack_index_pointer_requests") != 1:
            raise bench.BenchmarkError("warm open did not fetch exactly one inline catalog")
        if metrics.get("pack_index_requests") != 0:
            raise bench.BenchmarkError("warm open unexpectedly fetched a separate checkpoint")
        if metrics.get("pack_list_requests") != 0:
            raise bench.BenchmarkError("warm catalog open unexpectedly listed inventory")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--files", type=int, default=1024)
    parser.add_argument("--file-kib", type=int, default=32)
    parser.add_argument("--pack-target-mib", type=int, default=1)
    parser.add_argument("--warm-opens", type=int, default=5)
    parser.add_argument("--casita-bin", type=pathlib.Path, default=pathlib.Path("target/release/casita"))
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--keep-work", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--report", type=pathlib.Path)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if min(args.files, args.file_kib, args.pack_target_mib, args.warm_opens) < 1:
        raise SystemExit("benchmark sizes and --warm-opens must be positive")
    args.casita_bin = args.casita_bin.resolve()
    if not args.no_build:
        subprocess.run(
            ["cargo", "build", "--release", "--features", "cli", "--bin", "casita"],
            check=True,
        )
    if not args.casita_bin.exists():
        raise SystemExit(f"Casita binary does not exist: {args.casita_bin}")

    temporary = None
    if args.keep_work is None:
        temporary = tempfile.TemporaryDirectory(prefix="casita-pack-index-")
        workspace = pathlib.Path(temporary.name)
    else:
        workspace = args.keep_work.resolve()
        workspace.mkdir(parents=True, exist_ok=True)
    timestamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    output = args.output or pathlib.Path("benchmarks/results") / f"pack-index-{timestamp}.json"
    report = args.report or output.with_suffix(".md")
    env = {**os.environ, "CASITA_PACK_STATS": "1", "TZ": "UTC", "LC_ALL": "C"}

    try:
        source = workspace / "source"
        source.mkdir()
        for index in range(args.files):
            (source / f"file-{index:06d}.bin").write_bytes(
                bench.deterministic_bytes(f"pack-index-{index}", args.file_kib * KIB)
            )
        bench.set_fixed_metadata(source)
        repository = workspace / "repository"
        command = lambda *parts: [
            str(args.casita_bin),
            "--pack-target-bytes",
            str(args.pack_target_mib * MIB),
            "--repository",
            str(repository),
            *parts,
        ]
        bench.run_checked(command("init"), env=env)
        bench.run_checked(command("import", str(source), "--root", "bench/current"), env=env)
        packs = int(bench.CasitaAdapter(str(args.casita_bin)).storage_metrics([repository])["pack_count"])
        if packs < 1:
            raise bench.BenchmarkError("setup produced no packs")

        # Import may reopen the repository after sealing and publish a current
        # checkpoint itself. Remove only the advisory pointer so the first
        # measured process deterministically exercises footer reconstruction.
        (repository / "blobs" / "pack-index-current").unlink(missing_ok=True)
        cold = measure(command("root", "ls"), workspace, env, "cold")
        warm = [
            measure(command("root", "ls"), workspace, env, f"warm-{index + 1}")
            for index in range(args.warm_opens)
        ]
        validate(cold, warm, packs)
        result: dict[str, object] = {
            "result_schema": "casita.pack-index.v1",
            "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "configuration": {
                "files": args.files,
                "file_kib": args.file_kib,
                "pack_target_mib": args.pack_target_mib,
                "warm_opens": args.warm_opens,
            },
            "packs": packs,
            "cold": cold,
            "warm": warm,
        }
        bench.write_atomic(output, json.dumps(result, indent=2, sort_keys=True) + "\n")
        bench.write_atomic(report, render_report(result))
        print(f"result: {output}")
        print(f"report: {report}")
    finally:
        if temporary is not None:
            temporary.cleanup()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
