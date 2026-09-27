#!/usr/bin/env python3
"""Sweep Casita immutable-pack targets through the repository benchmark.

Each target gets an ordinary repository-suite result, so raw samples keep
the same validation and timing contract as the main suite. This driver adds a
compact cross-target report and preserves footer-tail request projections from
the pack files created by every sample.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
import pathlib
import statistics
import subprocess
import sys
from collections.abc import Sequence


MIB = 1024 * 1024


def positive_targets(value: str) -> list[int]:
    try:
        targets = [int(item.strip()) for item in value.split(",") if item.strip()]
    except ValueError as error:
        raise argparse.ArgumentTypeError("targets must be comma-separated integers") from error
    if not targets or any(target < 1 for target in targets):
        raise argparse.ArgumentTypeError("targets must contain positive MiB values")
    if len(set(targets)) != len(targets):
        raise argparse.ArgumentTypeError("targets must not contain duplicates")
    return targets


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    return ordered[max(0, math.ceil(len(ordered) * fraction) - 1)]


def median_metric(samples: list[dict[str, object]], group: str, key: str) -> float:
    values = [
        float(sample[group][key])
        for sample in samples
        if isinstance(sample.get(group), dict) and key in sample[group]
    ]
    return statistics.median(values) if values else 0.0


def observations(target_mib: int, result: dict[str, object]) -> list[dict[str, object]]:
    grouped: dict[tuple[str, str, str], list[dict[str, object]]] = {}
    for sample in result["samples"]:
        if sample["status"] != "ok" or sample["implementation"] != "casita":
            continue
        key = (sample["corpus"], sample["cache_policy"], sample["operation"])
        grouped.setdefault(key, []).append(sample)

    output = []
    for (corpus, cache, operation), samples in sorted(grouped.items()):
        wall = [float(sample["wall_seconds"]) for sample in samples]
        output.append(
            {
                "target_mib": target_mib,
                "corpus": corpus,
                "cache_policy": cache,
                "operation": operation,
                "samples": len(samples),
                "median_wall_seconds": statistics.median(wall),
                "p95_wall_seconds": percentile(wall, 0.95),
                "median_max_rss_bytes": statistics.median(
                    float(sample["max_rss_bytes"]) for sample in samples
                ),
                "median_repository_allocated_bytes": median_metric(
                    samples, "repository_usage", "allocated_bytes"
                ),
                "median_pack_count": median_metric(samples, "storage_metrics", "pack_count"),
                "median_pack_bytes": median_metric(samples, "storage_metrics", "pack_bytes"),
                "median_largest_pack_bytes": median_metric(
                    samples, "storage_metrics", "largest_pack_bytes"
                ),
                "median_pack_entries": median_metric(samples, "storage_metrics", "pack_entries"),
                "median_pack_footer_bytes": median_metric(
                    samples, "storage_metrics", "pack_footer_bytes"
                ),
                "median_footer_tail_miss_packs": median_metric(
                    samples, "storage_metrics", "footer_tail_miss_packs"
                ),
                "median_rebuild_gets_1m": median_metric(
                    samples, "storage_metrics", "rebuild_gets_tail_1024k"
                ),
                "median_rebuild_bytes_1m": median_metric(
                    samples, "storage_metrics", "rebuild_bytes_tail_1024k"
                ),
                "median_rebuild_gets_exact": median_metric(
                    samples, "storage_metrics", "rebuild_gets_exact"
                ),
                "median_rebuild_bytes_exact": median_metric(
                    samples, "storage_metrics", "rebuild_bytes_exact"
                ),
                "median_loose_chunk_count": median_metric(
                    samples, "storage_metrics", "loose_chunk_count"
                ),
            }
        )
    return output


def human_bytes(value: float) -> str:
    units = ("B", "KiB", "MiB", "GiB")
    unit = 0
    while value >= 1024 and unit < len(units) - 1:
        value /= 1024
        unit += 1
    return f"{value:.1f} {units[unit]}"


def render_report(result: dict[str, object]) -> str:
    lines = [
        "# Casita pack-limit sweep",
        "",
        "Times include the normal repository benchmark operation. Pack and footer metrics are read after the timed command.",
        "",
        "| Target | Corpus | Cache | Operation | n | Median | p95 | RSS | Allocation | Packs | Largest | Entries | Footer | Exact rebuild GETs | Exact rebuild bytes |",
        "|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for row in result["observations"]:
        lines.append(
            "| {target_mib} MiB | {corpus} | {cache_policy} | {operation} | {samples} | "
            "{median_wall_seconds:.4f} s | {p95_wall_seconds:.4f} s | {rss} | {allocated} | "
            "{packs:g} | {largest} | {entries:g} | {footer} | {exact_gets:g} | {exact_bytes} |".format(
                **row,
                rss=human_bytes(row["median_max_rss_bytes"]),
                allocated=human_bytes(row["median_repository_allocated_bytes"]),
                packs=row["median_pack_count"],
                largest=human_bytes(row["median_largest_pack_bytes"]),
                entries=row["median_pack_entries"],
                footer=human_bytes(row["median_pack_footer_bytes"]),
                exact_gets=row["median_rebuild_gets_exact"],
                exact_bytes=human_bytes(row["median_rebuild_bytes_exact"]),
            )
        )
    lines.extend(
        [
            "",
            "## Interpretation boundaries",
            "",
            "- Compare targets within the same corpus, cache policy, and operation.",
            "- Local timings do not predict S3 latency. Exact rebuild cost is two bounded range GETs per pack: a 16-byte trailer and the encoded footer.",
            "- Pack targets are approximate compressed-body limits and may be exceeded by one chunk.",
            "- A target that wins one operation is not automatically the default; storage, reopen, checkout, GC, and remote request costs form a Pareto trade-off.",
            "",
        ]
    )
    return "\n".join(lines)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--targets-mib", type=positive_targets, default=positive_targets("1,4,16,64,256"))
    parser.add_argument("--profile", choices=("smoke", "standard", "pack-tuning"), default="standard")
    parser.add_argument("--corpora", default="small-files,mixed,large-files")
    parser.add_argument("--operations", default="cold-import,checkout")
    parser.add_argument("--cache-policies", default="warm")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--seed", type=int, default=73)
    parser.add_argument("--casita-bin", type=pathlib.Path, default=pathlib.Path("target/release/casita"))
    parser.add_argument(
        "--casita-fsck-mode",
        choices=("audit-only", "dry-run"),
        default="audit-only",
        help="validation mode passed through to the repository suite",
    )
    parser.add_argument(
        "--skip-post-fsck",
        action="store_true",
        help="retain checkout/manifest validation without a redundant post-sample fsck",
    )
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--keep-work", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--report", type=pathlib.Path)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.repetitions < 1:
        raise SystemExit("--repetitions must be positive")
    timestamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    output = args.output or pathlib.Path("benchmarks/results") / f"pack-limits-{timestamp}.json"
    report = args.report or output.with_suffix(".md")
    raw_directory = output.with_suffix("").with_name(output.stem + "-raw")
    raw_directory.mkdir(parents=True, exist_ok=True)

    combined: list[dict[str, object]] = []
    raw_results: list[str] = []
    for index, target_mib in enumerate(args.targets_mib):
        raw = raw_directory / f"{target_mib}mib.json"
        command = [
            sys.executable,
            "-m",
            "benchmarks.suites.repository",
            "--profile",
            args.profile,
            "--corpora",
            args.corpora,
            "--operations",
            args.operations,
            "--implementations",
            "casita",
            "--cache-policies",
            args.cache_policies,
            "--repetitions",
            str(args.repetitions),
            "--seed",
            str(args.seed),
            "--casita-bin",
            str(args.casita_bin),
            "--casita-pack-target-bytes",
            str(target_mib * MIB),
            "--casita-fsck-mode",
            args.casita_fsck_mode,
            "--output",
            str(raw),
            "--report",
            str(raw.with_suffix(".md")),
        ]
        if args.no_build or index > 0:
            command.append("--no-build")
        if args.skip_post_fsck:
            command.append("--skip-post-fsck")
        if args.keep_work is not None:
            command.extend(["--keep-work", str(args.keep_work / f"{target_mib}mib")])
        print(f"[{index + 1}/{len(args.targets_mib)}] pack target {target_mib} MiB", flush=True)
        subprocess.run(command, check=True)
        raw_result = json.loads(raw.read_text())
        failed = [sample for sample in raw_result["samples"] if sample["status"] != "ok"]
        if failed:
            raise SystemExit(f"{target_mib} MiB run has {len(failed)} failed samples")
        combined.extend(observations(target_mib, raw_result))
        raw_results.append(str(raw))

    result: dict[str, object] = {
        "result_schema": "casita.pack-limits.v1",
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "configuration": {
            "targets_mib": args.targets_mib,
            "profile": args.profile,
            "corpora": args.corpora,
            "operations": args.operations,
            "cache_policies": args.cache_policies,
            "repetitions": args.repetitions,
            "seed": args.seed,
            "casita_fsck_mode": args.casita_fsck_mode,
            "skip_post_fsck": args.skip_post_fsck,
        },
        "raw_results": raw_results,
        "observations": combined,
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    report.write_text(render_report(result))
    print(f"summary: {output}")
    print(f"report: {report}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
