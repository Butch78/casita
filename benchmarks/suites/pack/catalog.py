#!/usr/bin/env python3
"""Scale the persistent pack catalog codec, memory, lookup, and GC operations."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
import os
import pathlib
import re
import statistics
import subprocess
import tempfile
from collections.abc import Sequence
from typing import Any

from benchmarks.lib.budgets import entrypoint_budgets
from benchmarks.suites import repository as bench
from benchmarks.suites.pack.gc import unique_positive_csv


PROBES = {
    "generate": "blob::pack::benchmarks::benchmark_index_catalog_scale",
    "decode": "blob::pack::benchmarks::benchmark_index_catalog_decode_only",
    "operations": "blob::pack::benchmarks::benchmark_index_operations_scale",
    "publication": "blob::pack::benchmarks::benchmark_index_catalog_publication_scale",
    "shard-generate": "blob::pack::benchmarks::benchmark_lazy_sharded_catalog_generate",
    "sharded": "blob::pack::benchmarks::benchmark_lazy_sharded_catalog_operations",
    "rebase": "blob::pack::benchmarks::benchmark_lazy_sharded_catalog_rebase",
    "run-lazy": "blob::pack::benchmarks::benchmark_lazy_sharded_catalog_run",
    "routing-scale": "blob::pack::benchmarks::benchmark_catalog_run_routing_scale",
}

CHUNK_LOCATION_BYTES = 96
SHARD_TARGET_BYTES = 32 * 1024 * 1024
PACK_ENTRY_BYTES = 32 + 8 + 8 + 8
RUN_CHUNK_BLOCK_ENTRIES = 1024
RUN_CHUNK_BLOCK_HEADER_BYTES = 16
RUN_CHUNK_BLOCK_REF_BYTES = 32 + 32 + 8 + 8 + 32
RUN_HEADER_BYTES = 8 + 8 + 8 + 32 + 8
RUN_QUERY_FIXED_BYTES = 8 + 8 + 8 + 56
RUN_ROUTING_FIXED_BYTES = 8 + 8 + 8
ROOT_RUN_REF_FIXED_BYTES = 1 + 32 + 8 + 8 + 8 + 1 + 8 + 8 + 32 + 8


def estimated_exact_delta_bytes_per_pack(chunks_per_pack: int) -> int:
    """Conservative inline exact-delta bytes for one live pack and no manifests."""
    footer_bytes = 8 + chunks_per_pack * PACK_ENTRY_BYTES
    tombstone_bitmap_bytes = (chunks_per_pack + 7) // 8
    checkpoint_bytes = (
        8  # checkpoint magic
        + 32  # inventory digest
        + 8  # pack count
        + 32  # pack digest
        + 8  # pack length
        + 8  # footer length
        + footer_bytes
        + 8  # tombstone bitmap length
        + tombstone_bitmap_bytes
        + 8  # tombstone record count
        + 8  # superseded pack count
        + 1  # manifests-complete flag
        + 8  # manifest count
    )
    return 8 + 8 + 8 + 8 + checkpoint_bytes


def estimated_queryable_run_bytes_per_pack(chunks_per_pack: int) -> int:
    """Conservative v2 queryable-run bytes for one live pack and no manifests."""
    delta_bytes = estimated_exact_delta_bytes_per_pack(chunks_per_pack)
    blocks = (chunks_per_pack + RUN_CHUNK_BLOCK_ENTRIES - 1) // RUN_CHUNK_BLOCK_ENTRIES
    query_bytes = (
        chunks_per_pack * CHUNK_LOCATION_BYTES
        + blocks * RUN_CHUNK_BLOCK_HEADER_BYTES
        + blocks * RUN_CHUNK_BLOCK_REF_BYTES
        + 32  # exact changed-pack ID
        + RUN_QUERY_FIXED_BYTES
    )
    return RUN_HEADER_BYTES + delta_bytes + query_bytes


def estimated_run_routing_bytes(*, packs: int, chunks: int, levels: int) -> int:
    """Estimate open-map exact routing for an aggregate run overlay."""
    blocks = (chunks + RUN_CHUNK_BLOCK_ENTRIES - 1) // RUN_CHUNK_BLOCK_ENTRIES
    return (
        levels * RUN_ROUTING_FIXED_BYTES
        + blocks * RUN_CHUNK_BLOCK_REF_BYTES
        + packs * 32
        + levels * ROOT_RUN_REF_FIXED_BYTES
    )


def expected_occupied_prefixes(prefixes: int, records: int) -> int:
    """Expected nonempty buckets for uniformly distributed content digests."""
    if prefixes < 1 or records < 0:
        raise ValueError("prefixes must be positive and records non-negative")
    if records == 0:
        return 0
    if prefixes == 1:
        return 1
    occupied = -prefixes * math.expm1(records * math.log1p(-1 / prefixes))
    return min(prefixes, math.ceil(occupied))


def request_amplification_projection(
    *,
    storage_tb: int,
    average_chunk_kib: int,
    pack_mib: int,
    run_mib: int,
) -> dict[str, int | float]:
    """Project requests if every bounded delta batch rewrites affected shards."""
    storage_bytes = storage_tb * 1_000_000_000_000
    chunk_bytes = average_chunk_kib * 1024
    pack_bytes = pack_mib * 1024 * 1024
    chunks = (storage_bytes + chunk_bytes - 1) // chunk_bytes
    required_shards = max(
        1,
        (chunks * CHUNK_LOCATION_BYTES + SHARD_TARGET_BYTES - 1)
        // SHARD_TARGET_BYTES,
    )
    shard_bits = max(1, (required_shards - 1).bit_length())
    prefixes = 1 << shard_bits
    chunks_per_pack = (pack_bytes + chunk_bytes - 1) // chunk_bytes
    delta_bytes_per_pack = estimated_exact_delta_bytes_per_pack(chunks_per_pack)
    run_target_bytes = run_mib * 1024 * 1024
    delta_packs = max(1, run_target_bytes // delta_bytes_per_pack)
    changed_chunks = chunks_per_pack * delta_packs
    chunk_prefixes = expected_occupied_prefixes(prefixes, changed_chunks)
    pack_prefixes = expected_occupied_prefixes(prefixes, delta_packs)
    shard_gets = chunk_prefixes + pack_prefixes
    # The new map is immutable. Its root is carried by the existing wal3 state
    # commit, so it needs no standalone catalog-pointer PUT.
    shard_puts = chunk_prefixes + pack_prefixes + 1
    rewrite_requests = shard_gets + shard_puts
    repository_packs = (storage_bytes + pack_bytes - 1) // pack_bytes
    batches = (repository_packs + delta_packs - 1) // delta_packs
    # Binary leveling keeps at most one run at each level. Every occupied-level
    # carry fetches one old run; the merged run and CAS pointer are one PUT each.
    immutable_run_merge_gets = batches - batches.bit_count()
    # The v1 root rides in the wal3 fragment already required by logical state.
    # Only sealed run PUTs and binary carry GETs are incremental catalog requests.
    catalog_pointer_puts = 0
    routing_map_puts = batches
    immutable_run_repository_requests = (
        batches + immutable_run_merge_gets + routing_map_puts
    )
    immutable_run_requests = immutable_run_repository_requests / batches
    shard_rewrite_repository_requests = batches * rewrite_requests
    return {
        "storage_bytes": storage_bytes,
        "chunks": chunks,
        "repository_packs": repository_packs,
        "shard_bits": shard_bits,
        "prefixes": prefixes,
        "chunks_per_pack": chunks_per_pack,
        "delta_bytes_per_pack": delta_bytes_per_pack,
        "run_target_bytes": run_target_bytes,
        "delta_packs": delta_packs,
        "changed_chunks": changed_chunks,
        "expected_chunk_prefixes": chunk_prefixes,
        "expected_pack_prefixes": pack_prefixes,
        "shard_rewrite_gets_per_batch": shard_gets,
        "shard_rewrite_puts_per_batch": shard_puts,
        "shard_rewrite_requests_per_batch": rewrite_requests,
        "immutable_run_requests_per_batch": immutable_run_requests,
        "request_amplification": (
            shard_rewrite_repository_requests / immutable_run_repository_requests
        ),
        "repository_batches": batches,
        "catalog_pointer_puts": catalog_pointer_puts,
        "wal3_embedded_catalog_publications": repository_packs,
        "shard_rewrite_repository_requests": shard_rewrite_repository_requests,
        "immutable_run_merge_gets": immutable_run_merge_gets,
        "routing_map_puts": routing_map_puts,
        "immutable_run_repository_requests": immutable_run_repository_requests,
    }


def periodic_rebase_projection(
    *,
    storage_tb: int,
    average_chunk_kib: int,
    pack_mib: int,
    run_mib: int,
    rebase_mib: int,
) -> dict[str, int | float]:
    """Project the complete-base requests made by production streaming rebases."""
    storage_bytes = storage_tb * 1_000_000_000_000
    chunk_bytes = average_chunk_kib * 1024
    pack_bytes = pack_mib * 1024 * 1024
    chunks = (storage_bytes + chunk_bytes - 1) // chunk_bytes
    repository_packs = (storage_bytes + pack_bytes - 1) // pack_bytes
    required_shards = max(
        1,
        (chunks * CHUNK_LOCATION_BYTES + SHARD_TARGET_BYTES - 1)
        // SHARD_TARGET_BYTES,
    )
    shard_bits = max(1, (required_shards - 1).bit_length())
    prefixes = 1 << shard_bits
    chunks_per_pack = (pack_bytes + chunk_bytes - 1) // chunk_bytes
    delta_bytes_per_pack = estimated_exact_delta_bytes_per_pack(chunks_per_pack)
    run_bytes_per_pack = estimated_queryable_run_bytes_per_pack(chunks_per_pack)
    rebase_target_bytes = rebase_mib * 1024 * 1024
    packs_per_rebase = max(1, rebase_target_bytes // run_bytes_per_pack)
    run_target_bytes = run_mib * 1024 * 1024
    packs_per_run = max(1, run_target_bytes // delta_bytes_per_pack)
    runs_per_rebase = (packs_per_rebase + packs_per_run - 1) // packs_per_run
    max_lookup_run_levels = max(1, (runs_per_rebase + 1).bit_length() - 1)
    routing_bytes = estimated_run_routing_bytes(
        packs=packs_per_rebase,
        chunks=packs_per_rebase * chunks_per_pack,
        levels=max_lookup_run_levels,
    )
    rebases = (repository_packs + packs_per_rebase - 1) // packs_per_rebase
    run_seals = (repository_packs + packs_per_run - 1) // packs_per_run
    # Binary levels are emptied by each complete-base rebase. Count carries
    # within those intervals instead of pretending that one unbounded level
    # set survives for the repository lifetime.
    remaining_packs = repository_packs
    run_merge_gets = 0
    while remaining_packs:
        interval_packs = min(remaining_packs, packs_per_rebase)
        interval_runs = (interval_packs + packs_per_run - 1) // packs_per_run
        run_merge_gets += interval_runs - interval_runs.bit_count()
        remaining_packs -= interval_packs
    chunk_shards = expected_occupied_prefixes(prefixes, chunks)
    pack_shards = expected_occupied_prefixes(prefixes, repository_packs)
    data_shards = chunk_shards + pack_shards
    gets_per_rebase = data_shards
    puts_per_rebase = data_shards + 1
    rebase_requests = rebases * (gets_per_rebase + puts_per_rebase)
    # Sealing writes one immutable run and, for a sharded base, one updated
    # authenticated routing map. Binary carries read the occupied old levels.
    run_object_puts = run_seals
    routing_map_puts = run_seals
    total_requests = (
        rebase_requests + run_merge_gets + run_object_puts + routing_map_puts
    )
    return {
        "storage_bytes": storage_bytes,
        "chunks": chunks,
        "repository_packs": repository_packs,
        "shard_bits": shard_bits,
        "prefixes": prefixes,
        "rebase_target_bytes": rebase_target_bytes,
        "run_bytes_per_pack": run_bytes_per_pack,
        "packs_per_rebase": packs_per_rebase,
        "packs_per_run": packs_per_run,
        "runs_per_rebase": runs_per_rebase,
        "max_lookup_run_levels": max_lookup_run_levels,
        "open_map_run_routing_bytes": routing_bytes,
        "rebases": rebases,
        "run_seals": run_seals,
        "run_merge_gets": run_merge_gets,
        "run_object_puts": run_object_puts,
        "routing_map_puts": routing_map_puts,
        "chunk_shards": chunk_shards,
        "pack_shards": pack_shards,
        "gets_per_rebase": gets_per_rebase,
        "puts_per_rebase": puts_per_rebase,
        "total_requests": total_requests,
        "rebase_requests": rebase_requests,
        "requests_per_pack": total_requests / repository_packs,
    }


def select_rebase_projection(
    projections: Sequence[dict[str, int | float]],
    *,
    max_routing_bytes: int = 32 * 1024 * 1024,
    max_lookup_run_levels: int = 12,
) -> dict[str, int | float]:
    """Select the largest threshold satisfying the in-root production limits."""
    eligible = [
        projection
        for projection in projections
        if projection["open_map_run_routing_bytes"] <= max_routing_bytes
        and projection["max_lookup_run_levels"] <= max_lookup_run_levels
    ]
    if not eligible:
        raise ValueError("rebase sweep has no point within production limits")
    return max(eligible, key=lambda projection: projection["rebase_target_bytes"])


def unique_percent_csv(value: str) -> list[int]:
    percentages: list[int] = []
    for item in value.split(","):
        try:
            percentage = int(item)
        except ValueError as error:
            raise argparse.ArgumentTypeError(
                f"invalid manifest percentage: {item!r}"
            ) from error
        if not 0 <= percentage <= 100:
            raise argparse.ArgumentTypeError(
                "manifest percentages must be between 0 and 100"
            )
        if percentage not in percentages:
            percentages.append(percentage)
    if not percentages:
        raise argparse.ArgumentTypeError("at least one manifest percentage is required")
    return percentages


def parse_metrics(output: str) -> dict[str, int]:
    metrics = {
        name: int(value)
        for name, value in re.findall(r"(?:^|\s)((?:catalog|index)_[a-z0-9_]+) (\d+)(?=\s|$)", output)
    }
    if "catalog_entries" not in metrics and "index_entries" not in metrics:
        raise RuntimeError(f"catalog probe emitted no metrics:\n{output}")
    return metrics


def parse_probe_binary(output: str) -> pathlib.Path:
    executables = []
    for line in output.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if (
            message.get("reason") == "compiler-artifact"
            and message.get("target", {}).get("name") == "casita"
            and message.get("profile", {}).get("test")
            and message.get("executable")
        ):
            executables.append(pathlib.Path(message["executable"]))
    if len(executables) != 1:
        raise RuntimeError(
            f"cargo emitted {len(executables)} catalog probe executables; expected one"
        )
    return executables[0]


def build_probe_binary() -> pathlib.Path:
    completed = subprocess.run(
        [
            "cargo",
            "test",
            "--release",
            "--features",
            "s3",
            "--lib",
            "--no-run",
            "--message-format=json",
        ],
        capture_output=True,
        text=True,
    )
    if completed.returncode:
        raise RuntimeError(completed.stderr or completed.stdout)
    executable = parse_probe_binary(completed.stdout)
    if not executable.is_file():
        raise RuntimeError(f"catalog probe binary does not exist: {executable}")
    return executable


def maximum_check(identifier: str, measured: int, limit: int, unit: str) -> dict[str, Any]:
    return {
        "id": identifier,
        "measured": measured,
        "limit": limit,
        "operator": "<=",
        "unit": unit,
        "status": "passed" if measured <= limit else "failed",
    }


def exact_check(identifier: str, measured: int, required: int, unit: str) -> dict[str, Any]:
    return {
        "id": identifier,
        "measured": measured,
        "limit": required,
        "operator": "==",
        "unit": unit,
        "status": "passed" if measured == required else "failed",
    }


def evaluate_lazy_budgets(
    sample: dict[str, Any], budgets: dict[str, int]
) -> list[dict[str, Any]]:
    generated = sample["shard_generate"]
    measured = sample["sharded"]
    checks = [
        exact_check(
            "lazy-open-map-get",
            measured["catalog_lazy_open_index_requests"],
            1,
            "requests",
        ),
        exact_check(
            "lazy-open-inventory-list",
            measured["catalog_lazy_open_list_requests"],
            0,
            "requests",
        ),
        exact_check(
            "lazy-open-footer-get",
            measured["catalog_lazy_open_footer_requests"],
            0,
            "requests",
        ),
        exact_check(
            "lazy-first-lookup-get",
            measured["catalog_lazy_first_lookup_requests"],
            2,
            "requests",
        ),
        maximum_check(
            "lazy-first-lookup-bytes",
            measured["catalog_lazy_first_lookup_bytes"],
            256 * 1024,
            "bytes",
        ),
        exact_check(
            "lazy-cached-lookup-get",
            measured["catalog_lazy_cached_lookup_requests"],
            0,
            "requests",
        ),
        exact_check(
            "lazy-chunk-list-gets",
            measured["catalog_lazy_chunk_list_requests"],
            generated["catalog_lazy_chunk_shards"],
            "requests",
        ),
        exact_check(
            "lazy-manifest-list-gets",
            measured["catalog_lazy_manifest_list_requests"],
            generated["catalog_lazy_manifest_shards"],
            "requests",
        ),
        exact_check(
            "lazy-gc-pack-shard-gets",
            measured["catalog_lazy_gc_requests"],
            generated["catalog_lazy_pack_shards"],
            "requests",
        ),
    ]
    if rebase := sample.get("rebase"):
        checks.extend(
            [
                exact_check(
                    "streaming-rebase-gets",
                    rebase["catalog_rebase_get_requests"],
                    rebase["catalog_rebase_old_objects"] + 1,
                    "requests",
                ),
                exact_check(
                    "streaming-rebase-puts",
                    rebase["catalog_rebase_put_requests"],
                    rebase["catalog_rebase_new_objects"] + 1,
                    "requests",
                ),
            ]
        )
    if run := sample.get("lazy_run"):
        checks.extend(
            [
                exact_check(
                    "lazy-run-open-get",
                    run["catalog_lazy_run_open_requests"],
                    1,
                    "requests",
                ),
                exact_check(
                    "lazy-run-first-use-get",
                    run["catalog_lazy_run_first_requests"],
                    1,
                    "requests",
                ),
                exact_check(
                    "lazy-run-cached-get",
                    run["catalog_lazy_run_cached_requests"],
                    0,
                    "requests",
                ),
            ]
        )
    if sample["entries"] == budgets["scale_entries"]:
        if rebase := sample.get("rebase"):
            checks.append(
                maximum_check(
                    "streaming-rebase-rss",
                    rebase["catalog_rebase_peak_rss_kib"] * 1024,
                    budgets["max_rebase_rss_bytes"],
                    "bytes",
                )
            )
        checks.append(
            maximum_check(
                "lazy-stream-rss",
                measured["catalog_lazy_stream_peak_rss_kib"] * 1024,
                budgets["max_lazy_reader_rss_bytes"],
                "bytes",
            )
        )
    return checks


def evaluate_budgets(
    sample: dict[str, Any], configuration: dict[str, Any], budgets: dict[str, int]
) -> list[dict[str, Any]]:
    if sample["entries"] != budgets["scale_entries"]:
        return []
    if sample["manifest_percent"] != 0:
        # Retained inline manifests have a separate, explicit incremental
        # allowance. Lazy readers keep their fixed budget at every density.
        allowance = sample["entries"] * sample["manifest_percent"] // 100 * 96
        return [maximum_check("reader-rss-with-manifests",
            sample["decode"]["catalog_peak_rss_kib"] * 1024,
            budgets["max_reader_rss_bytes"] + allowance, "bytes")]
    decoded = sample["decode"]
    operations = sample["operations"]
    checks = [
        maximum_check(
            "reader-rss",
            decoded["catalog_peak_rss_kib"] * 1024,
            budgets["max_reader_rss_bytes"],
            "bytes",
        ),
        maximum_check(
            "lookup-hit",
            operations["index_lookup_hit_nanos_per_op"],
            budgets["max_lookup_hit_nanos_per_op"],
            "ns/op",
        ),
        maximum_check(
            "lookup-miss",
            operations["index_lookup_miss_nanos_per_op"],
            budgets["max_lookup_miss_nanos_per_op"],
            "ns/op",
        ),
    ]
    if configuration["gc_percent"] == budgets["gc_percent"]:
        checks.append(
            maximum_check(
                f"gc-remove-{budgets['gc_percent']}-percent",
                operations["index_gc_median_nanos"],
                budgets["max_gc_nanos"],
                "ns",
            )
        )
    else:
        checks.append(
            {
                "id": f"gc-remove-{budgets['gc_percent']}-percent",
                "status": "not-applicable",
                "reason": (
                    f"run requested {configuration['gc_percent']}% removal; "
                    f"budget requires {budgets['gc_percent']}%"
                ),
            }
        )
    return checks


def summarize_budgets(samples: Sequence[dict[str, Any]]) -> dict[str, Any]:
    checks = [check for sample in samples for check in sample.get("budget_checks", [])]
    failed = [check for check in checks if check["status"] == "failed"]
    passed = [check for check in checks if check["status"] == "passed"]
    skipped = [check for check in checks if check["status"] == "not-applicable"]
    if failed:
        status = "failed"
    elif skipped or not checks:
        status = "partial" if checks else "not-applicable"
    else:
        status = "passed"
    return {
        "status": status,
        "passed": len(passed),
        "failed": len(failed),
        "not_applicable": len(skipped),
    }


def run_probe(
    executable: pathlib.Path,
    probe: str,
    entries: int,
    repetitions: int,
    catalog: pathlib.Path,
    *,
    lookups: int,
    threads: int,
    gc_percent: int,
    manifest_percent: int,
    shard_bits: int,
    shard_dir: pathlib.Path,
    shard_root: pathlib.Path,
    routing_packs: int | None = None,
    routing_chunks: int | None = None,
) -> dict[str, int]:
    environment = {
        **os.environ,
        "CASITA_CATALOG_BENCH_ENTRIES": str(entries),
        "CASITA_CATALOG_BENCH_REPETITIONS": str(repetitions),
        "CASITA_CATALOG_BENCH_LOOKUPS": str(lookups),
        "CASITA_CATALOG_BENCH_THREADS": str(threads),
        "CASITA_CATALOG_BENCH_GC_PERCENT": str(gc_percent),
        "CASITA_CATALOG_BENCH_MANIFEST_PERCENT": str(manifest_percent),
        "CASITA_CATALOG_BENCH_SHARD_BITS": str(shard_bits),
        "CASITA_CATALOG_BENCH_SHARD_DIR": str(shard_dir),
        "CASITA_CATALOG_BENCH_SHARD_ROOT": str(shard_root),
    }
    if routing_packs is not None:
        environment["CASITA_CATALOG_ROUTING_PACKS"] = str(routing_packs)
    if routing_chunks is not None:
        environment["CASITA_CATALOG_ROUTING_CHUNKS"] = str(routing_chunks)
    if probe == "generate":
        environment["CASITA_CATALOG_BENCH_OUTPUT"] = str(catalog)
    else:
        environment["CASITA_CATALOG_BENCH_INPUT"] = str(catalog)
    completed = subprocess.run(
        [
            str(executable),
            PROBES[probe],
            "--exact",
            "--ignored",
            "--nocapture",
        ],
        env=environment,
        capture_output=True,
        text=True,
    )
    if completed.returncode:
        raise RuntimeError(completed.stderr or completed.stdout)
    return parse_metrics(completed.stdout)


def aggregate_isolated_rebase_probes(
    probes: Sequence[dict[str, int]],
) -> dict[str, Any]:
    """Combine one-rebase processes without accumulating process VmHWM."""
    if not probes:
        raise ValueError("at least one isolated rebase probe is required")
    variable = {
        "catalog_rebase_median_nanos",
        "catalog_rebase_peak_rss_kib",
    }
    stable = {key: value for key, value in probes[0].items() if key not in variable}
    for probe in probes[1:]:
        observed = {key: value for key, value in probe.items() if key not in variable}
        if observed != stable:
            raise RuntimeError("isolated catalog rebase probes disagreed on exact metrics")
    result: dict[str, Any] = dict(probes[0])
    result["catalog_rebase_median_nanos"] = int(
        statistics.median(probe["catalog_rebase_median_nanos"] for probe in probes)
    )
    rss_samples = [probe["catalog_rebase_peak_rss_kib"] for probe in probes]
    result["catalog_rebase_peak_rss_kib"] = max(rss_samples)
    result["catalog_rebase_peak_rss_samples_kib"] = rss_samples
    result["catalog_rebase_isolated_processes"] = len(probes)
    return result


def render_report(result: dict[str, Any]) -> str:
    reader_threads = result["configuration"]["threads"]
    gc_percent = result["configuration"]["gc_percent"]
    lines = [
        "# Persistent catalog scale benchmark",
        "",
        "Codec and operation probes use the production catalog format in fresh release-mode test processes.",
        "",
        f"| Entries | Manifests | Catalog | Decode | Peak RSS | Chunk hit | Chunk miss | Manifest hit | Manifest miss | {reader_threads}-reader | List IDs | Remove {gc_percent}% |",
        "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for sample in result["samples"]:
        generated = sample["generate"]
        decoded = sample["decode"]
        operations = sample["operations"]
        lines.append(
            "| {entries:,} | {manifest_count:,} ({manifest_percent}%) | {catalog} | {decode:.3f} ms | {rss} | {hit} ns | {miss} ns | "
            "{manifest_hit} ns | {manifest_miss} ns | {parallel} ns | {listed:.3f} ms | {gc:.3f} ms |".format(
                entries=sample["entries"],
                manifest_count=generated["catalog_manifests"],
                manifest_percent=sample["manifest_percent"],
                catalog=bench.human_bytes(generated["catalog_bytes"]),
                decode=decoded["catalog_decode_median_nanos"] / 1_000_000,
                rss=bench.human_bytes(decoded["catalog_peak_rss_kib"] * 1024),
                hit=operations["index_lookup_hit_nanos_per_op"],
                miss=operations["index_lookup_miss_nanos_per_op"],
                manifest_hit=operations["index_manifest_lookup_hit_nanos_per_op"],
                manifest_miss=operations["index_manifest_lookup_miss_nanos_per_op"],
                parallel=operations["index_parallel_lookup_nanos_per_op"],
                listed=operations["index_list_median_nanos"] / 1_000_000,
                gc=operations["index_gc_median_nanos"] / 1_000_000,
            )
        )
    if result["samples"] and "sharded" in result["samples"][0]:
        lines.extend(
            [
                "",
                "## Measured lazy sharded-base path",
                "",
                "These fresh-process probes open the production sharded root from a state catalog, fetch authenticated routing and candidate blocks for a cold point lookup, reuse the bounded cache, and stream chunk, manifest, and pack-state shards without inventory or pack-footer recovery.",
                "",
                "| Entries | Bits | Root | Map | Open | Open RSS | First lookup | Cached lookup | Chunk stream | Manifest stream | Forced GC | Stream RSS |",
                "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
            ]
        )
        for sample in result["samples"]:
            generated = sample["shard_generate"]
            measured = sample["sharded"]
            lines.append(
                "| {entries:,} | {bits} | {root} | {map} | {open:.3f} ms / {open_gets} GET | {open_rss} | "
                "{first:.3f} ms / {first_gets} GET | {cached:.3f} ms / {cached_gets} GET | "
                "{chunks:.3f} ms / {chunk_gets} GET | {manifests:.3f} ms / {manifest_gets} GET | "
                "{gc:.3f} ms / {gc_gets} GET | {stream_rss} |".format(
                    entries=sample["entries"],
                    bits=generated["catalog_lazy_shard_bits"],
                    root=bench.human_bytes(generated["catalog_lazy_root_bytes"]),
                    map=bench.human_bytes(generated["catalog_lazy_map_bytes"]),
                    open=measured["catalog_lazy_open_median_nanos"] / 1_000_000,
                    open_gets=measured["catalog_lazy_open_index_requests"],
                    open_rss=bench.human_bytes(
                        measured["catalog_lazy_open_peak_rss_kib"] * 1024
                    ),
                    first=measured["catalog_lazy_first_lookup_median_nanos"] / 1_000_000,
                    first_gets=measured["catalog_lazy_first_lookup_requests"],
                    cached=measured["catalog_lazy_cached_lookup_median_nanos"] / 1_000_000,
                    cached_gets=measured["catalog_lazy_cached_lookup_requests"],
                    chunks=measured["catalog_lazy_chunk_list_median_nanos"] / 1_000_000,
                    chunk_gets=measured["catalog_lazy_chunk_list_requests"],
                    manifests=measured["catalog_lazy_manifest_list_median_nanos"] / 1_000_000,
                    manifest_gets=measured["catalog_lazy_manifest_list_requests"],
                    gc=measured["catalog_lazy_gc_median_nanos"] / 1_000_000,
                    gc_gets=measured["catalog_lazy_gc_requests"],
                    stream_rss=bench.human_bytes(
                        measured["catalog_lazy_stream_peak_rss_kib"] * 1024
                    ),
                )
            )
        if all("rebase" in sample for sample in result["samples"]):
            lines.extend(
                [
                    "",
                "## Measured streaming rebase",
                "",
                "The production rebase folds an exact overlay into a lazy immutable base one shard at a time. Each repetition runs in a fresh process so the RSS gate measures one rebase rather than allocator high-water retained by earlier repetitions. The probe adds a 2,048-chunk pack; request counts include every old data shard read and every new data shard plus map written, but no inventory LIST or pack-footer recovery.",
                    "",
                    "| Entries | Added chunks | Time | Peak RSS | GETs | Read | PUTs | Written |",
                    "|---:|---:|---:|---:|---:|---:|---:|---:|",
                ]
            )
            for sample in result["samples"]:
                rebase = sample["rebase"]
                lines.append(
                    "| {entries:,} | {added:,} | {time:.3f} ms | {rss} | {gets:,} | {read} | {puts:,} | {written} |".format(
                        entries=sample["entries"],
                        added=rebase["catalog_rebase_added_chunks"],
                        time=rebase["catalog_rebase_median_nanos"] / 1_000_000,
                        rss=bench.human_bytes(rebase["catalog_rebase_peak_rss_kib"] * 1024),
                        gets=rebase["catalog_rebase_get_requests"],
                        read=bench.human_bytes(rebase["catalog_rebase_get_bytes"]),
                        puts=rebase["catalog_rebase_put_requests"],
                        written=bench.human_bytes(rebase["catalog_rebase_put_bytes"]),
                    )
                )
        if all("lazy_run" in sample for sample in result["samples"]):
            lines.extend(
                [
                    "",
                    "## Measured deferred immutable run",
                    "",
                    "A sharded root that references an immutable run still opens with only its map GET. That authenticated map contains the block routing while the WAL3 root stays small, so the first point lookup fetches only one candidate chunk block; subsequent lookups reuse the bounded cache without materializing the run.",
                    "",
                    "| Entries | Run | Routing | Open map | WAL3 root | Open | First use | First transfer | Cached | Peak RSS |",
                    "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
                ]
            )
            for sample in result["samples"]:
                run = sample["lazy_run"]
                lines.append(
                    "| {entries:,} | {size} | {routing} | {map_size} | {root_size} | {open:.3f} ms / {open_gets} GET | {first:.3f} ms / {first_gets} GET | {transfer} | {cached:.3f} ms / {cached_gets} GET | {rss} |".format(
                        entries=sample["entries"],
                        size=bench.human_bytes(run["catalog_lazy_run_bytes"]),
                        routing=bench.human_bytes(run["catalog_lazy_run_routing_bytes"]),
                        map_size=bench.human_bytes(run["catalog_lazy_run_map_bytes"]),
                        root_size=bench.human_bytes(run["catalog_lazy_run_root_bytes"]),
                        open=run["catalog_lazy_run_open_nanos"] / 1_000_000,
                        open_gets=run["catalog_lazy_run_open_requests"],
                        first=run["catalog_lazy_run_first_nanos"] / 1_000_000,
                        first_gets=run["catalog_lazy_run_first_requests"],
                        transfer=bench.human_bytes(run["catalog_lazy_run_first_bytes"]),
                        cached=run["catalog_lazy_run_cached_nanos"] / 1_000_000,
                        cached_gets=run["catalog_lazy_run_cached_requests"],
                        rss=bench.human_bytes(run["catalog_lazy_run_peak_rss_kib"] * 1024),
                    )
                )
    projection = result.get("request_projection")
    routing_scale = result.get("routing_scale")
    if routing_scale is not None:
        lines.extend(
            [
                "",
                "## Measured 500 TB open-map routing",
                "",
                "This materializes the selected 4 GiB rebase interval's exact production routing shape: every changed pack ID and every 1,024-chunk block reference. It measures authenticated shard-map encoding, map decoding, routing-index decoding, and process peak RSS instead of extrapolating a tiny map.",
                "",
                "| Packs | Chunks | Blocks | Routing | Map | Encode | Map decode | Routing decode | Peak RSS |",
                "|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
                "| {packs:,} | {chunks:,} | {blocks:,} | {routing} | {map_size} | {encode:.3f} ms | {map_decode:.3f} ms | {routing_decode:.3f} ms | {rss} |".format(
                    packs=routing_scale["catalog_routing_packs"],
                    chunks=routing_scale["catalog_routing_chunks"],
                    blocks=routing_scale["catalog_routing_blocks"],
                    routing=bench.human_bytes(routing_scale["catalog_routing_bytes"]),
                    map_size=bench.human_bytes(routing_scale["catalog_routing_map_bytes"]),
                    encode=routing_scale["catalog_routing_encode_median_nanos"] / 1_000_000,
                    map_decode=routing_scale["catalog_routing_map_decode_median_nanos"] / 1_000_000,
                    routing_decode=routing_scale["catalog_routing_decode_median_nanos"] / 1_000_000,
                    rss=bench.human_bytes(routing_scale["catalog_routing_peak_rss_kib"] * 1024),
                ),
            ]
        )
    if projection is not None:
        lines.extend(
            [
                "",
                "## S3 request amplification projection",
                "",
                "This is an analytical uniform-digest workload model, not a measured S3 result. It tests the proposed strategy of fetching and rewriting every affected base shard when a delta batch is compacted.",
                "",
                "| Storage | Shard bits | Packs per batch | Changed chunks | Chunk shards | Pack shards | GETs | PUTs | Requests | Incremental run requests | Amplification |",
                "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
                "| {storage} | {bits} | {packs:,} | {chunks:,} | {chunk_shards:,} | {pack_shards:,} | {gets:,} | {puts:,} | {requests:,} | {run_requests:.2f} | {amplification:.1f}× |".format(
                    storage=bench.human_bytes(projection["storage_bytes"]),
                    bits=projection["shard_bits"],
                    packs=projection["delta_packs"],
                    chunks=projection["changed_chunks"],
                    chunk_shards=projection["expected_chunk_prefixes"],
                    pack_shards=projection["expected_pack_prefixes"],
                    gets=projection["shard_rewrite_gets_per_batch"],
                    puts=projection["shard_rewrite_puts_per_batch"],
                    requests=projection["shard_rewrite_requests_per_batch"],
                    run_requests=projection["immutable_run_requests_per_batch"],
                    amplification=projection["request_amplification"],
                ),
                "",
                "A high value rejects eager affected-shard rewriting as the steady-state publication protocol. Production v1 carries ordinary roots in wal3 at zero additional requests, seals byte-bounded immutable runs into binary levels, and reads sharded bases lazily.",
            ]
        )
    rebase_projections = result.get("rebase_projections")
    if rebase_projections:
        selected_rebase = result.get("selected_rebase_projection") or select_rebase_projection(
            rebase_projections
        )
        lines.extend(
            [
                "",
                "## Production periodic-rebase threshold sweep",
                "",
                "This is an analytical zero-manifest, uniform-digest model of complete-base streaming rebases. Streaming bounds memory, but every rebase reads every old chunk and pack-state shard and writes every new shard plus one map. Exact run routing is stored in that authenticated map, so opening still uses one GET and WAL3 stays small; map transfer size and cold run probes constrain how far the threshold can grow.",
                "",
                "| Policy | Storage | Threshold | Rebase count | Packs/rebase | Run levels | Open-map routing | GETs/rebase | PUTs/rebase | Run GETs | Run PUTs | Routing PUTs | Total requests | Requests/pack |",
                "|:---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
            ]
        )
        for rebase_projection in rebase_projections:
            lines.append(
                "| {policy} | {storage} | {threshold} | {rebases:,} | {packs:,} | {levels} | {routing} | {gets:,} | {puts:,} | {run_gets:,} | {run_puts:,} | {routing_puts:,} | {total:,} | {per_pack:.3f} |".format(
                    policy=(
                        "selected"
                        if rebase_projection["rebase_target_bytes"]
                        == selected_rebase["rebase_target_bytes"]
                        else ""
                    ),
                    storage=bench.human_bytes(rebase_projection["storage_bytes"]),
                    threshold=bench.human_bytes(rebase_projection["rebase_target_bytes"]),
                    rebases=rebase_projection["rebases"],
                    packs=rebase_projection["packs_per_rebase"],
                    levels=rebase_projection["max_lookup_run_levels"],
                    routing=bench.human_bytes(rebase_projection["open_map_run_routing_bytes"]),
                    gets=rebase_projection["gets_per_rebase"],
                    puts=rebase_projection["puts_per_rebase"],
                    run_gets=rebase_projection["run_merge_gets"],
                    run_puts=rebase_projection["run_object_puts"],
                    routing_puts=rebase_projection["routing_map_puts"],
                    total=rebase_projection["total_requests"],
                    per_pack=rebase_projection["requests_per_pack"],
                )
            )
        lines.extend(
            [
                "",
                "Production selects the largest threshold that keeps authenticated open-map run routing at or below 32 MiB and cold run depth at or below 12 levels.",
            ]
        )
    lines.extend(
        [
            "",
            "## Immutable shard layout",
            "",
            "The same exact catalog is split into independent chunk-location, manifest-membership, and pack-state tables. Object counts include only nonempty prefixes; the map is one immutable content-addressed object.",
            "",
            "| Entries | Prefix bits | Map | Objects | Chunk | Manifest | Pack | Largest shard | Total shard bytes | Encode |",
            "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for sample in result["samples"]:
        publication = sample["publication"]
        lines.append(
            "| {entries:,} | {bits} | {map_bytes} | {objects:,} | {chunks:,} | {manifests:,} | {packs:,} | {largest} | {total} | {encode:.3f} ms |".format(
                entries=sample["entries"],
                bits=publication["catalog_shard_bits"],
                map_bytes=bench.human_bytes(publication["catalog_shard_map_bytes"]),
                objects=publication["catalog_shard_objects"],
                chunks=publication["catalog_shard_chunk_objects"],
                manifests=publication["catalog_shard_manifest_objects"],
                packs=publication["catalog_shard_pack_objects"],
                largest=bench.human_bytes(publication["catalog_shard_max_object_bytes"]),
                total=bench.human_bytes(publication["catalog_shard_object_bytes"]),
                encode=publication["catalog_shard_encode_nanos"] / 1_000_000,
            )
        )
    lines.extend(
        [
            "",
            "Point metrics are medians over the configured repetitions. The parallel column is aggregate nanoseconds per lookup.",
            "GC includes requested-digest preparation and in-memory removal; it excludes remote compaction I/O.",
            "",
            "## Catalog v1 publication",
            "",
            "The exact-mutation probe adds one pack and 1,024 manifests to the generated catalog. It compares a full inline rewrite with v1's content-addressed base plus one inline delta; this forced external-base mode requires two GETs on cold reopen, while small live repositories retain a one-GET inline base.",
            "",
            "| Entries | Manifests | Full rewrite | Delta root | Reduction | Full encode | Delta encode | Delta reopen | Reopen transfer | GETs |",
            "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for sample in result["samples"]:
        publication = sample["publication"]
        full = publication["catalog_full_rewrite_bytes"]
        delta = publication["catalog_delta_catalog_bytes"]
        lines.append(
            "| {entries:,} | {manifest_percent}% | {full} | {delta} | {reduction:.1f}× | {full_encode:.3f} ms | {delta_encode:.3f} ms | {reopen:.3f} ms | {transfer} | {requests} |".format(
                entries=sample["entries"],
                manifest_percent=sample["manifest_percent"],
                full=bench.human_bytes(full),
                delta=bench.human_bytes(delta),
                reduction=full / delta,
                full_encode=publication["catalog_full_encode_median_nanos"] / 1_000_000,
                delta_encode=publication["catalog_delta_encode_median_nanos"] / 1_000_000,
                reopen=publication["catalog_delta_reopen_median_nanos"] / 1_000_000,
                transfer=bench.human_bytes(publication["catalog_delta_reopen_bytes"]),
                requests=publication["catalog_delta_reopen_requests"],
            )
        )
    lines.extend(
        [
            "",
            "## Leveled run sealing",
            "",
            "When the inline delta byte bound is reached, the production v1 path merges that batch into one immutable content-addressed run and publishes a small root. Standalone stores CAS-update the advisory pointer; WAL3-backed repositories carry the root in the logical state checkpoint. Cold reopen reads the root, base, and run; additional occupied binary levels add at most one immutable object each.",
            "",
            "| Entries | Deltas sealed | Run object | Root | Seal encode | Reopen | Cold GETs |",
            "|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for sample in result["samples"]:
        publication = sample["publication"]
        lines.append(
            "| {entries:,} | {deltas:,} | {run} | {root} | {seal:.3f} ms | {reopen:.3f} ms | {requests} |".format(
                entries=sample["entries"],
                deltas=publication["catalog_run_batch_deltas"],
                run=bench.human_bytes(publication["catalog_run_bytes"]),
                root=bench.human_bytes(publication["catalog_run_catalog_bytes"]),
                seal=publication["catalog_run_seal_median_nanos"] / 1_000_000,
                reopen=publication["catalog_run_reopen_median_nanos"] / 1_000_000,
                requests=publication["catalog_run_reopen_requests"],
            )
        )
    lines.append("")
    lines.extend(
        [
            "## Acceptance budgets",
            "",
            f"Overall status: **{result['budget_summary']['status']}**.",
            "",
            "| Entries | Manifests | Check | Measured | Limit | Status |",
            "|---:|---:|---|---:|---:|---|",
        ]
    )
    for sample in result["samples"]:
        for check in sample.get("budget_checks", []):
            if check["status"] == "not-applicable":
                lines.append(
                    f"| {sample['entries']:,} | {sample['manifest_percent']}% | "
                    f"{check['id']} | — | — | not applicable |"
                )
                continue
            if check["unit"] == "bytes":
                measured = bench.human_bytes(check["measured"])
                limit = bench.human_bytes(check["limit"])
            elif check["unit"] == "ns":
                measured = f"{check['measured'] / 1_000_000:.3f} ms"
                limit = f"{check['limit'] / 1_000_000:.3f} ms"
            elif check["unit"] == "requests":
                measured = str(check["measured"])
                limit = str(check["limit"])
            else:
                measured = f"{check['measured']} ns/op"
                limit = f"{check['limit']} ns/op"
            lines.append(
                f"| {sample['entries']:,} | {sample['manifest_percent']}% | "
                f"{check['id']} | {measured} | {limit} | {check['status']} |"
            )
    for check in result.get("routing_budget_checks", []):
        measured = bench.human_bytes(check["measured"])
        limit = bench.human_bytes(check["limit"])
        lines.append(
            f"| 500 TB | — | {check['id']} | {measured} | {limit} | {check['status']} |"
        )
    lines.append("")
    return "\n".join(lines)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--entries", default="65536,262144,1000000")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--lookups", type=int, default=1_000_000)
    parser.add_argument("--threads", type=int, default=8)
    parser.add_argument("--gc-percent", type=int, default=10)
    parser.add_argument(
        "--shard-bits",
        type=int,
        default=0,
        help="digest-prefix width for the shard-layout probe (0 selects the 32 MiB target)",
    )
    parser.add_argument("--projection-storage-tb", type=int, default=500)
    parser.add_argument("--projection-average-chunk-kib", type=int, default=256)
    parser.add_argument("--projection-pack-mib", type=int, default=16)
    parser.add_argument("--projection-run-mib", type=int, default=1)
    parser.add_argument("--projection-rebase-mibs", default="64,256,1024,4096")
    parser.add_argument(
        "--manifest-percents",
        default="0",
        help="comma-separated manifest counts as percentages of chunk entries",
    )
    parser.add_argument("--catalog-dir", type=pathlib.Path)
    parser.add_argument(
        "--probe-binary",
        type=pathlib.Path,
        help="prebuilt release lib-test binary; otherwise Cargo builds it once",
    )
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--report", type=pathlib.Path)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    entries = unique_positive_csv(args.entries)
    rebase_mibs = unique_positive_csv(args.projection_rebase_mibs)
    manifest_percents = unique_percent_csv(args.manifest_percents)
    if min(args.repetitions, args.lookups, args.threads) < 1:
        raise SystemExit("--repetitions, --lookups, and --threads must be positive")
    if not 1 <= args.gc_percent <= 100:
        raise SystemExit("--gc-percent must be between 1 and 100")
    if not 0 <= args.shard_bits <= 24:
        raise SystemExit("--shard-bits must be between 0 and 24")
    if min(
        args.projection_storage_tb,
        args.projection_average_chunk_kib,
        args.projection_pack_mib,
        args.projection_run_mib,
    ) < 1:
        raise SystemExit("request-projection inputs must be positive")
    probe_binary = (
        args.probe_binary.resolve() if args.probe_binary is not None else build_probe_binary()
    )
    if not probe_binary.is_file():
        raise SystemExit(f"catalog probe binary does not exist: {probe_binary}")
    budgets = entrypoint_budgets("catalog-index")
    configuration = {
        "entries": entries,
        "repetitions": args.repetitions,
        "lookups": args.lookups,
        "threads": args.threads,
        "gc_percent": args.gc_percent,
        "manifest_percents": manifest_percents,
        "shard_bits": args.shard_bits,
        "projection_storage_tb": args.projection_storage_tb,
        "projection_average_chunk_kib": args.projection_average_chunk_kib,
        "projection_pack_mib": args.projection_pack_mib,
        "projection_run_mib": args.projection_run_mib,
        "projection_rebase_mibs": rebase_mibs,
    }
    request_projection = request_amplification_projection(
        storage_tb=args.projection_storage_tb,
        average_chunk_kib=args.projection_average_chunk_kib,
        pack_mib=args.projection_pack_mib,
        run_mib=args.projection_run_mib,
    )
    rebase_projections = [
        periodic_rebase_projection(
            storage_tb=args.projection_storage_tb,
            average_chunk_kib=args.projection_average_chunk_kib,
            pack_mib=args.projection_pack_mib,
            run_mib=args.projection_run_mib,
            rebase_mib=rebase_mib,
        )
        for rebase_mib in rebase_mibs
    ]
    selected_rebase = select_rebase_projection(rebase_projections)
    timestamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    output = args.output or pathlib.Path("benchmarks/results") / f"catalog-index-{timestamp}.json"
    report = args.report or output.with_suffix(".md")
    temporary = None
    if args.catalog_dir is None:
        temporary = tempfile.TemporaryDirectory(prefix="casita-catalog-benchmark-")
        catalog_dir = pathlib.Path(temporary.name)
    else:
        catalog_dir = args.catalog_dir
        catalog_dir.mkdir(parents=True, exist_ok=True)

    samples = []
    try:
        combinations = [
            (count, manifest_percent)
            for count in entries
            for manifest_percent in manifest_percents
        ]
        for ordinal, (count, manifest_percent) in enumerate(combinations, start=1):
            print(
                f"[{ordinal}/{len(combinations)}] entries={count} manifests={manifest_percent}%",
                flush=True,
            )
            catalog = catalog_dir / f"catalog-{count}-manifests-{manifest_percent}"
            generated = run_probe(
                probe_binary, "generate", count, args.repetitions, catalog,
                lookups=args.lookups, threads=args.threads, gc_percent=args.gc_percent,
                manifest_percent=manifest_percent,
                shard_bits=args.shard_bits,
                shard_dir=catalog_dir / f"shards-{count}-manifests-{manifest_percent}",
                shard_root=catalog_dir / f"sharded-root-{count}-manifests-{manifest_percent}",
            )
            shard_dir = catalog_dir / f"shards-{count}-manifests-{manifest_percent}"
            shard_root = catalog_dir / f"sharded-root-{count}-manifests-{manifest_percent}"
            decoded = run_probe(
                probe_binary, "decode", count, args.repetitions, catalog,
                lookups=args.lookups, threads=args.threads, gc_percent=args.gc_percent,
                manifest_percent=manifest_percent,
                shard_bits=args.shard_bits,
                shard_dir=shard_dir,
                shard_root=shard_root,
            )
            operations = run_probe(
                probe_binary, "operations", count, args.repetitions, catalog,
                lookups=args.lookups, threads=args.threads, gc_percent=args.gc_percent,
                manifest_percent=manifest_percent,
                shard_bits=args.shard_bits,
                shard_dir=shard_dir,
                shard_root=shard_root,
            )
            publication = run_probe(
                probe_binary, "publication", count, args.repetitions, catalog,
                lookups=args.lookups, threads=args.threads, gc_percent=args.gc_percent,
                manifest_percent=manifest_percent,
                shard_bits=args.shard_bits,
                shard_dir=shard_dir,
                shard_root=shard_root,
            )
            shard_generated = run_probe(
                probe_binary, "shard-generate", count, args.repetitions, catalog,
                lookups=args.lookups, threads=args.threads, gc_percent=args.gc_percent,
                manifest_percent=manifest_percent, shard_bits=args.shard_bits,
                shard_dir=shard_dir, shard_root=shard_root,
            )
            sharded = run_probe(
                probe_binary, "sharded", count, args.repetitions, catalog,
                lookups=args.lookups, threads=args.threads, gc_percent=args.gc_percent,
                manifest_percent=manifest_percent, shard_bits=args.shard_bits,
                shard_dir=shard_dir, shard_root=shard_root,
            )
            rebase = aggregate_isolated_rebase_probes(
                [
                    run_probe(
                        probe_binary, "rebase", count, 1, catalog,
                        lookups=args.lookups, threads=args.threads,
                        gc_percent=args.gc_percent,
                        manifest_percent=manifest_percent,
                        shard_bits=args.shard_bits,
                        shard_dir=shard_dir, shard_root=shard_root,
                    )
                    for _ in range(args.repetitions)
                ]
            )
            lazy_run = run_probe(
                probe_binary, "run-lazy", count, args.repetitions, catalog,
                lookups=args.lookups, threads=args.threads, gc_percent=args.gc_percent,
                manifest_percent=manifest_percent, shard_bits=args.shard_bits,
                shard_dir=shard_dir, shard_root=shard_root,
            )
            sample = {
                "entries": count,
                "manifest_percent": manifest_percent,
                "generate": generated,
                "decode": decoded,
                "operations": operations,
                "publication": publication,
                "shard_generate": shard_generated,
                "sharded": sharded,
                "rebase": rebase,
                "lazy_run": lazy_run,
            }
            sample["budget_checks"] = evaluate_budgets(
                sample, configuration, budgets
            ) + evaluate_lazy_budgets(sample, budgets)
            samples.append(sample)
        routing_packs = int(selected_rebase["packs_per_rebase"])
        routing_chunks = routing_packs * int(request_projection["chunks_per_pack"])
        routing_scale = run_probe(
            probe_binary,
            "routing-scale",
            routing_chunks,
            args.repetitions,
            catalog_dir / "routing-scale-unused",
            lookups=args.lookups,
            threads=args.threads,
            gc_percent=args.gc_percent,
            manifest_percent=0,
            shard_bits=args.shard_bits,
            shard_dir=catalog_dir / "routing-scale-unused-shards",
            shard_root=catalog_dir / "routing-scale-unused-root",
            routing_packs=routing_packs,
            routing_chunks=routing_chunks,
        )
        routing_checks = [
            maximum_check(
                "500tb-routing-bytes",
                routing_scale["catalog_routing_bytes"],
                32 * 1024 * 1024,
                "bytes",
            ),
            maximum_check(
                "500tb-routing-rss",
                routing_scale["catalog_routing_peak_rss_kib"] * 1024,
                budgets["max_lazy_reader_rss_bytes"],
                "bytes",
            ),
        ]
        budget_summary = summarize_budgets(
            [*samples, {"budget_checks": routing_checks}]
        )
        result = {
            "result_schema": "casita.catalog-index.v1",
            "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "configuration": configuration,
            "budgets": budgets,
            "budget_summary": budget_summary,
            "request_projection": request_projection,
            "rebase_projections": rebase_projections,
            "selected_rebase_projection": selected_rebase,
            "routing_scale": routing_scale,
            "routing_budget_checks": routing_checks,
            "samples": samples,
        }
        bench.write_atomic(output, json.dumps(result, indent=2, sort_keys=True) + "\n")
        bench.write_atomic(report, render_report(result))
        print(f"result: {output}")
        print(f"report: {report}")
        return int(budget_summary["status"] == "failed")
    finally:
        if temporary is not None:
            temporary.cleanup()


if __name__ == "__main__":
    raise SystemExit(main())
