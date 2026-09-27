"""Repeated edits versus fresh packing of identical final bytes."""
from __future__ import annotations

import argparse
import hashlib
import itertools
import json
import os
import pathlib
import subprocess

from benchmarks import cli
from benchmarks.suites import repository as common
from benchmarks.suites.metadata_collection import CARGO_ARGUMENTS
from benchmarks.suites.pack.catalog import parse_probe_binary

PROBE = "blob::pack::fragmentation::benchmark_pack_fragmentation"
CORRECTNESS = "exact bytes and independent BLAKE3; all historical versions verified"
CACHES = ("disabled", "below-largest-pack", "fits-largest-pack", "above-largest-pack", "fits-working-set")


def parse_samples(stdout, points):
    samples = [json.loads(line.removeprefix("fragmentation_sample "))
               for line in stdout.splitlines() if line.startswith("fragmentation_sample ")]
    expected = set(itertools.product(("localized", "scattered"), points,
                                    ("history", "fresh"), CACHES, ("cold", "warm")))
    seen = set()
    identities = {}
    for row in samples:
        key = tuple(row.get(field) for field in ("pattern", "generation", "layout", "cache", "phase"))
        if key not in expected or key in seen or row.get("correctness") != CORRECTNESS:
            raise common.BenchmarkError("duplicate, unexpected, or unverified fragmentation sample")
        seen.add(key)
        for field in ("nanos", "file_bytes", "chunks", "referenced_packs", "pack_runs",
                      "referenced_pack_bytes", "largest_pack_bytes", "stored_bytes", "stored_pack_bytes"):
            if type(row.get(field)) is not int or row[field] <= 0:
                raise common.BenchmarkError(f"invalid fragmentation metric: {field}")
        for field in ("pack_requests", "pack_read_bytes", "chunk_range_requests", "whole_pack_requests",
                      "cache_hits", "cache_promotions", "cache_evictions", "index_requests", "index_bytes"):
            if type(row.get(field)) is not int or row[field] < 0:
                raise common.BenchmarkError(f"invalid fragmentation counter: {field}")
        capacities = dict(zip(CACHES, (0, row["largest_pack_bytes"] - 1, row["largest_pack_bytes"],
                                       row["largest_pack_bytes"] + 1, row["referenced_pack_bytes"])))
        if (row.get("cache_bytes") != capacities[row["cache"]]
                or row["file_bytes"] != 16 * 1024 * 1024
                or row.get("pack_target_bytes") != 1024 * 1024
                or row.get("avg_chunk_bytes") != 256 * 1024
                or not isinstance(row.get("blob"), str) or not row["blob"]):
            raise common.BenchmarkError("wrong fragmentation fixture or cache boundary")
        identity_key = (row["pattern"], row["generation"])
        if identities.setdefault(identity_key, row["blob"]) != row["blob"]:
            raise common.BenchmarkError("fresh and historical layouts contain different bytes")
    if seen != expected or "test result: ok. 1 passed; 0 failed;" not in stdout:
        raise common.BenchmarkError("incomplete or failed fragmentation matrix")
    return samples


def report(result):
    lines = ["# Pack fragmentation", "", f"Complete: {result['complete']}", "",
             "In-memory origin; cold/warm refer to Casita handles and pack caches. No simulated network latency.",
             "16 MiB random files, 4 KiB replacements, 256 KiB average CDC, 1 MiB pack target, serial chunk uploads.",
             "", "| Pattern | Edits | Layout | Cache | Phase | Rep | Packs | Runs | Pack GETs | Read MiB | ms |",
             "|---|---:|---|---|---|---:|---:|---:|---:|---:|---:|"]
    for s in result["samples"]:
        lines.append(f"| {s['pattern']} | {s['generation']} | {s['layout']} | {s['cache']} | {s['phase']} | "
                     f"{s['repetition']} | {s['referenced_packs']} | {s['pack_runs']} | {s['pack_requests']} | "
                     f"{s['pack_read_bytes'] / 2**20:.3f} | {s['nanos'] / 1e6:.3f} |")
    if "error" in result:
        lines += ["", result["error"]]
    return "\n".join(lines) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--probe-binary", type=pathlib.Path)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--output", type=pathlib.Path, default=cli.ROOT / "benchmarks/results/pack-fragmentation.json")
    parser.add_argument("--report", type=pathlib.Path)
    args = parser.parse_args(argv)
    if args.repetitions < 1 or (args.no_build and args.probe_binary is None):
        parser.error("positive repetitions and a binary for --no-build are required")
    points = [0, 1, 4] if args.profile == "smoke" else [0, 1, 4, 16, 32]
    binary = args.probe_binary
    if binary is None:
        built = subprocess.run(["cargo", *CARGO_ARGUMENTS], cwd=cli.ROOT, capture_output=True, text=True)
        if built.returncode:
            raise common.BenchmarkError(built.stderr or built.stdout)
        binary = parse_probe_binary(built.stdout)
    binary = binary.resolve()
    with binary.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    result = dict(schema_version=1, result_schema="casita.pack-fragmentation.v1", suite_id="blob-backends",
                  complete=False, environment=common.environment_metadata(cli.ROOT),
                  configuration=dict(profile=args.profile, generations=points, repetitions=args.repetitions,
                                     origin="in-memory", edit_bytes=4096, chunk_upload_concurrency=1),
                  artifacts=[dict(path=str(binary), sha256=digest)], samples=[], processes=[])

    def save():
        common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
        common.write_atomic(args.report or args.output.with_suffix(".md"), report(result))

    save()
    try:
        for repetition in range(1, args.repetitions + 1):
            process = subprocess.run([str(binary), PROBE, "--exact", "--ignored", "--nocapture"],
                env={**os.environ, "CASITA_FRAGMENTATION_GENERATIONS": ",".join(map(str, points))},
                capture_output=True, text=True)
            result["processes"].append(dict(repetition=repetition, exit_code=process.returncode,
                                           stdout=process.stdout, stderr=process.stderr))
            if process.returncode:
                raise common.BenchmarkError("fragmentation probe failed")
            for sample in parse_samples(process.stdout, points):
                result["samples"].append(dict(sample, repetition=repetition, status="ok"))
            save()
        result["complete"] = True
    except Exception as error:
        result["error"] = str(error)
        raise
    finally:
        save()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
