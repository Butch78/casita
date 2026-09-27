#!/usr/bin/env python3
"""Sweep Casita immutable-pack read settings against an S3 endpoint.

The benchmark uploads one repository per pack target below a unique run prefix,
then repeatedly syncs that same repository into fresh local destinations while
sweeping compressed-chunk cache capacities. It intentionally
does not delete remote data; the emitted result lists every prefix for explicit
cleanup under the operator's normal bucket-retention policy.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import pathlib
import statistics
import subprocess
import sys
import tempfile
import uuid
from collections.abc import Sequence

from benchmarks.suites import repository as bench


MIB = 1024 * 1024


def positive_csv(value: str, *, allow_zero: bool = False) -> list[int]:
    try:
        values = [int(item.strip()) for item in value.split(",") if item.strip()]
    except ValueError as error:
        raise argparse.ArgumentTypeError("values must be comma-separated integers") from error
    minimum = 0 if allow_zero else 1
    if not values or any(item < minimum for item in values):
        qualifier = "non-negative" if allow_zero else "positive"
        raise argparse.ArgumentTypeError(f"values must be {qualifier}")
    if len(values) != len(set(values)):
        raise argparse.ArgumentTypeError("values must not contain duplicates")
    return values


def s3_url(value: str) -> str:
    if not value.startswith("s3://") or value == "s3://" or "?" in value or "#" in value:
        raise argparse.ArgumentTypeError("expected s3://BUCKET/PREFIX")
    return value.rstrip("/")


def cli(
    executable: pathlib.Path,
    target_mib: int,
    cache_mib: int,
    *args: str,
) -> list[str]:
    return [
        str(executable),
        "--pack-target-bytes",
        str(target_mib * MIB),
        "--pack-cache-bytes",
        str(cache_mib * MIB),
        *args,
    ]


def render_report(result: dict[str, object]) -> str:
    grouped: dict[tuple[int, int], list[dict[str, object]]] = {}
    for sample in result["samples"]:
        key = (
            int(sample["target_mib"]),
            int(sample["cache_mib"]),
        )
        grouped.setdefault(key, []).append(sample)

    lines = [
        "# Casita S3 pack sweep",
        "",
        f"Latency label: `{result['configuration']['latency_label']}`. Times include S3 index open, closure transfer, and local publication.",
        "",
        "| Target | Cache | n | Median | p95 | Source ranges | Whole GETs | Cache hits | Footer GETs | Footer bytes |",
        "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for key, samples in sorted(grouped.items()):
        walls = sorted(float(sample["wall_seconds"]) for sample in samples)
        p95 = bench.percentile(walls, 0.95)

        def median_metric(name: str) -> float:
            return statistics.median(
                float(sample["operation_metrics"].get(name, 0)) for sample in samples
            )

        target, cache = key
        lines.append(
            f"| {target} MiB | {cache} MiB | {len(samples)} | "
            f"{statistics.median(walls):.4f} s | {p95:.4f} s | "
            f"{median_metric('source_pack_chunk_range_requests'):g} | "
            f"{median_metric('source_pack_whole_requests'):g} | "
            f"{median_metric('source_pack_cache_hits'):g} | "
            f"{median_metric('source_pack_footer_range_requests'):g} | "
            f"{bench.human_bytes(int(median_metric('source_pack_footer_range_bytes')))} |"
        )
    lines.extend(
        [
            "",
            "## Interpretation boundaries",
            "",
            "- Compare rows only within the same endpoint, latency label, corpus, and run window.",
            "- Cache `0 MiB` disables payload caching.",
            "- Every timed sample starts a new process and local destination. Cache hits measure reuse within one closure transfer, not a long-lived daemon.",
            "- Remote prefixes are retained and listed in the raw JSON; cleanup is explicit and outside this benchmark.",
            "",
        ]
    )
    return "\n".join(lines)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--s3-url", required=True, type=s3_url)
    parser.add_argument("--targets-mib", default="4,16,32")
    parser.add_argument("--cache-mib", default="0,64,256")
    parser.add_argument("--profile", choices=sorted(bench.SCALES), default="standard")
    parser.add_argument("--corpus", choices=bench.CORPORA, default="mixed")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--latency-label", default="endpoint-native")
    parser.add_argument("--run-id", default=None)
    parser.add_argument("--casita-bin", type=pathlib.Path, default=pathlib.Path("target/release/casita"))
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--keep-work", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--report", type=pathlib.Path)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    targets = positive_csv(args.targets_mib)
    caches = positive_csv(args.cache_mib, allow_zero=True)
    if args.repetitions < 1:
        raise SystemExit("--repetitions must be positive")
    args.casita_bin = args.casita_bin.resolve()
    if not args.no_build:
        subprocess.run(
            ["cargo", "build", "--release", "--features", "cli,s3", "--bin", "casita"],
            check=True,
        )
    if not args.casita_bin.exists():
        raise SystemExit(f"Casita binary does not exist: {args.casita_bin}")

    timestamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    run_id = args.run_id or f"{timestamp}-{uuid.uuid4().hex[:8]}"
    output = args.output or pathlib.Path("benchmarks/results") / f"s3-pack-{run_id}.json"
    report = args.report or output.with_suffix(".md")
    env = {**os.environ, "CASITA_PACK_STATS": "1", "TZ": "UTC", "LC_ALL": "C"}
    adapter = bench.CasitaAdapter(str(args.casita_bin))

    temporary = None
    if args.keep_work is None:
        temporary = tempfile.TemporaryDirectory(prefix="casita-s3-pack-")
        work = pathlib.Path(temporary.name)
    else:
        work = args.keep_work.resolve()
        work.mkdir(parents=True, exist_ok=True)
    corpus = bench.generate_corpus(work / "corpus", args.corpus, bench.SCALES[args.profile][args.corpus])
    source = work / "source"
    bench.run_checked([str(args.casita_bin), "--repository", str(source), "init"], env=env)
    bench.run_checked(
        [
            str(args.casita_bin),
            "--pack-target-bytes",
            str(targets[0] * MIB),
            "--repository",
            str(source),
            "import",
            str(corpus.base),
            "--root",
            "bench/current",
        ],
        env=env,
    )

    samples: list[dict[str, object]] = []
    remote_prefixes: list[str] = []
    try:
        for target in targets:
            remote = f"{args.s3_url}/{run_id}/target-{target}mib"
            remote_prefixes.append(remote)
            seed = cli(
                args.casita_bin,
                target,
                0,
                "sync",
                "--from",
                str(source),
                "--to",
                remote,
                "--root",
                "bench/current",
                "--writer",
                f"bench-seed-{run_id}",
            )
            bench.run_checked(seed, env=env)

            for cache in caches:
                for repetition in range(args.repetitions):
                    destination = work / f"destination-{target}-{cache}-{repetition}"
                    bench.run_checked(
                        [str(args.casita_bin), "--repository", str(destination), "init"], env=env
                    )
                    stdout = work / f"stdout-{target}-{cache}-{repetition}.txt"
                    stderr = work / f"stderr-{target}-{cache}-{repetition}.txt"
                    command = cli(
                        args.casita_bin,
                        target,
                        cache,
                        "sync",
                        "--from",
                        remote,
                        "--to",
                        str(destination),
                        "--root",
                        "bench/current",
                        "--writer",
                        f"bench-read-{run_id}-{repetition}",
                    )
                    timing = bench.measured_command(
                        bench.CommandSpec([command], work, env), stdout, stderr
                    )
                    stdout_text = stdout.read_text(errors="replace")
                    root = adapter.root_key(destination)
                    restored = work / f"restored-{target}-{cache}-{repetition}"
                    bench.run_checked(
                        adapter.command(destination, "checkout", root, str(restored), "--no-root"),
                        env=env,
                    )
                    bench.assert_manifest(restored, corpus.base_manifest)
                    sample: dict[str, object] = {
                        "target_mib": target,
                        "cache_mib": cache,
                        "repetition": repetition,
                        "operation_metrics": adapter.operation_metrics("sync-cold", stdout_text),
                        **timing,
                    }
                    samples.append(sample)
                    print(
                        f"target={target}MiB cache={cache}MiB "
                        f"rep={repetition + 1}/{args.repetitions} {timing['wall_seconds']:.4f}s",
                        flush=True,
                    )

        result: dict[str, object] = {
            "result_schema": "casita.s3-pack.v1",
            "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "configuration": {
                "s3_url": args.s3_url,
                "run_id": run_id,
                "targets_mib": targets,
                "cache_mib": caches,
                "profile": args.profile,
                "corpus": args.corpus,
                "repetitions": args.repetitions,
                "latency_label": args.latency_label,
            },
            "remote_prefixes": remote_prefixes,
            "samples": samples,
        }
        bench.write_atomic(output, json.dumps(result, indent=2, sort_keys=True) + "\n")
        bench.write_atomic(report, render_report(result))
        print(f"result: {output}")
        print(f"report: {report}")
        print("remote prefixes retained for explicit cleanup:")
        for prefix in remote_prefixes:
            print(f"  {prefix}")
    finally:
        if temporary is not None:
            temporary.cleanup()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
