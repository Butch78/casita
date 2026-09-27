#!/usr/bin/env python3
"""Benchmark immutable-pack GC across controlled dead-data densities.

Each sample imports a deterministic fixed-size-file tree, removes an evenly
distributed percentage of files, publishes the retained tree, and times only
``casita gc``. Pack footers captured immediately before GC and replacement or
batched tombstone records captured afterward reveal the *actual* density of
every dirty pack. The harness validates the request shape chosen at each
density.
"""

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


MIB = 1024 * 1024
KIB = 1024
PACK_TRAILER_BYTES = 16
PACK_ENTRY_BYTES = 56
PACK_MAGIC = b"casitac1"
REPLACEMENT_MAGIC = b"casitar1"
TOMBSTONE_MAGIC = b"casitat1"


def unique_positive_csv(value: str, *, maximum: int | None = None) -> list[int]:
    try:
        values = [int(item.strip()) for item in value.split(",") if item.strip()]
    except ValueError as error:
        raise argparse.ArgumentTypeError("values must be comma-separated integers") from error
    if not values or any(item < 1 or (maximum is not None and item > maximum) for item in values):
        suffix = f" no greater than {maximum}" if maximum is not None else ""
        raise argparse.ArgumentTypeError(f"values must be positive integers{suffix}")
    if len(values) != len(set(values)):
        raise argparse.ArgumentTypeError("values must not contain duplicates")
    return values


def pack_inventory(repository: pathlib.Path) -> dict[str, dict[str, object]]:
    root = repository / "blobs" / "packs" / "b3"
    packs: dict[str, dict[str, object]] = {}
    if not root.exists():
        return packs
    for path in sorted(candidate for candidate in root.rglob("*") if candidate.is_file()):
        size = path.stat().st_size
        if size < PACK_TRAILER_BYTES:
            raise bench.BenchmarkError(f"truncated pack: {path}")
        with path.open("rb") as handle:
            handle.seek(-PACK_TRAILER_BYTES, os.SEEK_END)
            trailer = handle.read(PACK_TRAILER_BYTES)
            footer_bytes = int.from_bytes(trailer[:8], "little")
            if trailer[8:] != PACK_MAGIC or footer_bytes < 8 or footer_bytes + 16 > size:
                raise bench.BenchmarkError(f"invalid pack trailer: {path}")
            handle.seek(-(PACK_TRAILER_BYTES + footer_bytes), os.SEEK_END)
            footer = handle.read(footer_bytes)
            entries = int.from_bytes(footer[:8], "little")
            if footer_bytes != 8 + entries * PACK_ENTRY_BYTES:
                raise bench.BenchmarkError(f"invalid pack footer length: {path}")
            decoded_entries = []
            for ordinal in range(entries):
                at = 8 + ordinal * PACK_ENTRY_BYTES
                decoded_entries.append(
                    {
                        "digest": footer[at : at + 32].hex(),
                        "framed_bytes": int.from_bytes(footer[at + 40 : at + 48], "little"),
                    }
                )
        packs[path.name] = {
            "bytes": size,
            "body_bytes": size - footer_bytes - PACK_TRAILER_BYTES,
            "entries": entries,
            "footer_entries": decoded_entries,
        }
    return packs


def replacement_inventory(repository: pathlib.Path) -> dict[str, str | None]:
    root = repository / "blobs" / "pack-replacements" / "b3"
    replacements: dict[str, str | None] = {}
    if not root.exists():
        return replacements
    for path in sorted(candidate for candidate in root.rglob("*") if candidate.is_file()):
        marker = path.read_bytes()
        if len(marker) < 41 or marker[:8] != REPLACEMENT_MAGIC:
            raise bench.BenchmarkError(f"invalid replacement marker: {path}")
        old = marker[8:40].hex()
        flag = marker[40]
        if flag == 0 and len(marker) == 41:
            new = None
        elif flag == 1 and len(marker) == 73:
            new = marker[41:73].hex()
        else:
            raise bench.BenchmarkError(f"invalid replacement marker length: {path}")
        if old in replacements and replacements[old] != new:
            raise bench.BenchmarkError(f"conflicting replacement markers for pack {old}")
        replacements[old] = new
    return replacements


def tombstone_inventory(repository: pathlib.Path) -> dict[str, set[int]]:
    root = repository / "blobs" / "pack-tombstones" / "b3"
    tombstones: dict[str, set[int]] = {}
    if not root.exists():
        return tombstones

    def add(pack: str, entry_count: int, bitmap: bytes, path: pathlib.Path) -> None:
        if len(bitmap) != (entry_count + 7) // 8:
            raise bench.BenchmarkError(f"invalid tombstone bitmap length: {path}")
        dead = tombstones.setdefault(pack, set())
        dead.update(
            ordinal
            for ordinal in range(entry_count)
            if bitmap[ordinal // 8] & (1 << (ordinal % 8))
        )

    for path in sorted(candidate for candidate in root.rglob("*") if candidate.is_file()):
        marker = path.read_bytes()
        if len(marker) >= 48 and marker[:8] == TOMBSTONE_MAGIC:
            pack = marker[8:40].hex()
            entry_count = int.from_bytes(marker[40:48], "little")
            add(pack, entry_count, marker[48:], path)
            continue
        if len(marker) < 16 or marker[:8] != b"casitad1":
            raise bench.BenchmarkError(f"invalid tombstone marker: {path}")
        count = int.from_bytes(marker[8:16], "little")
        at = 16
        for _ in range(count):
            if at + 40 > len(marker):
                raise bench.BenchmarkError(f"truncated tombstone delta: {path}")
            pack = marker[at : at + 32].hex()
            entry_count = int.from_bytes(marker[at + 32 : at + 40], "little")
            bitmap_len = (entry_count + 7) // 8
            at += 40
            add(pack, entry_count, marker[at : at + bitmap_len], path)
            at += bitmap_len
        if count == 0 or at != len(marker):
            raise bench.BenchmarkError(f"invalid tombstone delta length: {path}")
    return tombstones


def evenly_distributed_indexes(count: int, percent: int) -> list[int]:
    dead = max(1, round(count * percent / 100))
    return [(index * count) // dead for index in range(dead)]


def summarize_gc(
    before: dict[str, dict[str, object]],
    after: dict[str, dict[str, object]],
    replacements: dict[str, str | None],
    tombstones: dict[str, set[int]],
    operation_metrics: dict[str, int],
) -> dict[str, int | float]:
    dirty_entries = 0
    removed_entries = 0
    dirty_body_bytes = 0
    removed_body_bytes = 0
    replacement_bytes = 0
    rewritten = 0
    dead = 0
    deferred = 0
    for old, new in replacements.items():
        if old not in before:
            raise bench.BenchmarkError(f"replacement references unknown old pack {old}")
        if old in after:
            raise bench.BenchmarkError(f"superseded pack {old} remains present")
        old_pack = before[old]
        new_pack = {"entries": 0, "body_bytes": 0, "bytes": 0}
        if new is None:
            dead += 1
        else:
            rewritten += 1
            if new not in after:
                raise bench.BenchmarkError(f"replacement pack {new} is absent")
            new_pack = after[new]
            replacement_bytes += new_pack["bytes"]
        dirty_entries += old_pack["entries"]
        removed_entries += old_pack["entries"] - new_pack["entries"]
        dirty_body_bytes += old_pack["body_bytes"]
        removed_body_bytes += old_pack["body_bytes"] - new_pack["body_bytes"]

    for pack, ordinals in tombstones.items():
        if pack in replacements:
            continue
        if pack not in before or pack not in after:
            raise bench.BenchmarkError(f"tombstone references unavailable pack {pack}")
        old_pack = before[pack]
        entries = old_pack["footer_entries"]
        if any(ordinal >= len(entries) for ordinal in ordinals):
            raise bench.BenchmarkError(f"tombstone ordinal exceeds footer for pack {pack}")
        deferred += 1
        dirty_entries += old_pack["entries"]
        removed_entries += len(ordinals)
        dirty_body_bytes += old_pack["body_bytes"]
        removed_body_bytes += sum(entries[ordinal]["framed_bytes"] for ordinal in ordinals)

    whole_requests = operation_metrics.get("pack_whole_requests", 0)
    range_requests = operation_metrics.get("pack_chunk_range_requests", 0)
    replacement_puts = operation_metrics.get("pack_gc_replacement_put_requests", 0)
    replacement_put_bytes = operation_metrics.get("pack_gc_replacement_put_bytes", 0)
    marker_puts = operation_metrics.get("pack_gc_marker_put_requests", 0)
    pack_deletes = operation_metrics.get("pack_gc_delete_requests", 0)
    tombstone_puts = operation_metrics.get("pack_gc_tombstone_put_requests", 0)
    tombstone_put_bytes = operation_metrics.get("pack_gc_tombstone_put_bytes", 0)
    deferred_packs = operation_metrics.get("pack_gc_deferred_packs", 0)
    if range_requests != 0:
        raise bench.BenchmarkError(f"GC made {range_requests} per-chunk range requests")
    if whole_requests != rewritten:
        raise bench.BenchmarkError(
            f"GC made {whole_requests} whole reads for {rewritten} partially live packs"
        )
    if replacement_puts != rewritten or replacement_put_bytes != replacement_bytes:
        raise bench.BenchmarkError(
            f"GC reported {replacement_puts} replacement PUTs/{replacement_put_bytes} bytes, "
            f"pack inventory found {rewritten}/{replacement_bytes}"
        )
    if marker_puts != len(replacements) or pack_deletes != len(replacements):
        raise bench.BenchmarkError(
            f"GC reported {marker_puts} marker PUTs and {pack_deletes} deletes for "
            f"{len(replacements)} dirty packs"
        )
    if tombstone_puts != int(deferred > 0) or deferred_packs != deferred:
        raise bench.BenchmarkError(
            f"GC reported {tombstone_puts} tombstone PUTs/{deferred_packs} deferred packs, "
            f"inventory found {deferred}"
        )

    reclaimed_pack_bytes = sum(item["bytes"] for item in before.values()) - sum(
        item["bytes"] for item in after.values()
    )
    whole_bytes = operation_metrics.get("pack_whole_bytes", 0)
    return {
        "dirty_packs": len(replacements) + deferred,
        "dead_packs": dead,
        "rewritten_packs": rewritten,
        "deferred_packs": deferred,
        "tombstone_put_requests": tombstone_puts,
        "dirty_entries": dirty_entries,
        "removed_entries": removed_entries,
        "actual_dead_percent": 100.0 * removed_entries / dirty_entries if dirty_entries else 0.0,
        "dirty_body_bytes": dirty_body_bytes,
        "removed_body_bytes": removed_body_bytes,
        "whole_read_bytes": whole_bytes,
        "replacement_write_bytes": replacement_put_bytes,
        "tombstone_write_bytes": tombstone_put_bytes,
        "reclaimed_pack_bytes": reclaimed_pack_bytes,
        "read_amplification": whole_bytes / removed_body_bytes if removed_body_bytes else 0.0,
        "write_amplification": replacement_bytes / removed_body_bytes if removed_body_bytes else 0.0,
    }


def generate_files(root: pathlib.Path, count: int, size: int) -> None:
    root.mkdir(parents=True)
    for index in range(count):
        path = root / f"file-{index:06d}.bin"
        path.write_bytes(bench.deterministic_bytes(f"pack-gc-{index}", size))
    bench.set_fixed_metadata(root)


def render_report(result: dict[str, object]) -> str:
    grouped: dict[tuple[int, int], list[dict[str, object]]] = {}
    for sample in result["samples"]:
        grouped.setdefault(
            (int(sample["target_mib"]), int(sample["requested_dead_percent"])), []
        ).append(sample)
    lines = [
        "# Casita pack GC density sweep",
        "",
        "Times include repository open, mark/sweep, pack replacement, and logical commit. Setup and validation are outside the timed command.",
        "",
        "| Target | Requested dead | Actual dirty-pack dead | n | Median | p95 | Dirty | Defer | Delta PUTs | Drop | Rewrite | Read | Pack write | Tombstone write | Reclaimed | Read amp | Write amp |",
        "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for (target, requested), samples in sorted(grouped.items()):
        walls = [float(sample["wall_seconds"]) for sample in samples]

        def median(name: str) -> float:
            return statistics.median(float(sample["gc_metrics"][name]) for sample in samples)

        lines.append(
            f"| {target} MiB | {requested}% | {median('actual_dead_percent'):.1f}% | "
            f"{len(samples)} | {statistics.median(walls):.4f} s | "
            f"{bench.percentile(walls, 0.95):.4f} s | {median('dirty_packs'):g} | "
            f"{median('deferred_packs'):g} | {median('tombstone_put_requests'):g} | "
            f"{median('dead_packs'):g} | "
            f"{median('rewritten_packs'):g} | "
            f"{bench.human_bytes(int(median('whole_read_bytes')))} | "
            f"{bench.human_bytes(int(median('replacement_write_bytes')))} | "
            f"{bench.human_bytes(int(median('tombstone_write_bytes')))} | "
            f"{bench.human_bytes(int(median('reclaimed_pack_bytes')))} | "
            f"{median('read_amplification'):.2f}x | {median('write_amplification'):.2f}x |"
        )
    lines.extend(
        [
            "",
            "## Validated invariants",
            "",
            "- Fully dead packs are deleted without a whole-pack GET.",
            "- Sparse dirty packs below 50% publish only a durable bitmap tombstone and perform no whole-pack GET.",
            "- Denser partially live dirty packs have exactly one whole-pack GET and one replacement pack.",
            "- GC performs no per-survivor chunk range reads.",
            "- Every replacement marker names an old pack that disappeared and, when present, a new pack that exists.",
            "",
            "Actual density includes metadata chunks and is derived from old and replacement pack footers; it need not equal the requested file-deletion percentage.",
            "",
        ]
    )
    return "\n".join(lines)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--targets-mib", default="4,16")
    parser.add_argument("--dead-percent", default="1,10,50,100")
    parser.add_argument("--files", type=int, default=512)
    parser.add_argument("--file-kib", type=int, default=64)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--casita-bin", type=pathlib.Path, default=pathlib.Path("target/release/casita"))
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--keep-work", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--report", type=pathlib.Path)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    targets = unique_positive_csv(args.targets_mib)
    densities = unique_positive_csv(args.dead_percent, maximum=100)
    if args.files < 1 or args.file_kib < 1 or args.repetitions < 1:
        raise SystemExit("--files, --file-kib, and --repetitions must be positive")
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
        temporary = tempfile.TemporaryDirectory(prefix="casita-pack-gc-")
        work = pathlib.Path(temporary.name)
    else:
        work = args.keep_work.resolve()
        work.mkdir(parents=True, exist_ok=True)
    timestamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    output = args.output or pathlib.Path("benchmarks/results") / f"pack-gc-{timestamp}.json"
    report = args.report or output.with_suffix(".md")
    env = {**os.environ, "CASITA_PACK_STATS": "1", "TZ": "UTC", "LC_ALL": "C"}
    adapter = bench.CasitaAdapter(str(args.casita_bin))
    samples: list[dict[str, object]] = []
    try:
        total = len(targets) * len(densities) * args.repetitions
        sample_index = 0
        for target in targets:
            for density in densities:
                for repetition in range(1, args.repetitions + 1):
                    sample_index += 1
                    print(
                        f"[{sample_index}/{total}] target={target}MiB dead={density}% repetition={repetition}",
                        flush=True,
                    )
                    workspace = work / f"target-{target}-dead-{density}-r{repetition}"
                    workspace.mkdir(parents=True)
                    source = workspace / "source"
                    repository = workspace / "repository"
                    generate_files(source, args.files, args.file_kib * KIB)
                    command = lambda *parts: [
                        str(args.casita_bin),
                        "--pack-target-bytes",
                        str(target * MIB),
                        "--repository",
                        str(repository),
                        *parts,
                    ]
                    bench.run_checked(command("init"), env=env)
                    bench.run_checked(
                        command("import", str(source), "--root", "bench/current"), env=env
                    )
                    removed = evenly_distributed_indexes(args.files, density)
                    for index in removed:
                        (source / f"file-{index:06d}.bin").unlink()
                    bench.run_checked(
                        command(
                            "import",
                            str(source),
                            "--root",
                            "bench/current",
                            "--filesystem-rehash",
                        ),
                        env=env,
                    )
                    before = pack_inventory(repository)
                    stdout_path = workspace / "gc.stdout"
                    stderr_path = workspace / "gc.stderr"
                    timing = bench.measured_command(
                        bench.CommandSpec([command("gc")], workspace, env),
                        stdout_path,
                        stderr_path,
                    )
                    stdout = stdout_path.read_text(errors="replace")
                    metrics = adapter.operation_metrics("collect", stdout)
                    after = pack_inventory(repository)
                    replacements = replacement_inventory(repository)
                    tombstones = tombstone_inventory(repository)
                    gc_metrics = summarize_gc(
                        before, after, replacements, tombstones, metrics
                    )
                    bench.run_checked(command("fsck"), env=env)
                    restored = workspace / "restored"
                    root = adapter.root_key(repository)
                    bench.run_checked(
                        command("checkout", root, str(restored), "--no-root"), env=env
                    )
                    if len(list(restored.glob("file-*.bin"))) != args.files - len(removed):
                        raise bench.BenchmarkError("restored file count does not match retained tree")
                    samples.append(
                        {
                            "target_mib": target,
                            "requested_dead_percent": density,
                            "repetition": repetition,
                            "files": args.files,
                            "removed_files": len(removed),
                            "file_bytes": args.file_kib * KIB,
                            "operation_metrics": metrics,
                            "gc_metrics": gc_metrics,
                            **timing,
                        }
                    )
        result: dict[str, object] = {
            "result_schema": "casita.pack-gc.v1",
            "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "configuration": {
                "targets_mib": targets,
                "dead_percent": densities,
                "files": args.files,
                "file_kib": args.file_kib,
                "repetitions": args.repetitions,
            },
            "samples": samples,
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
