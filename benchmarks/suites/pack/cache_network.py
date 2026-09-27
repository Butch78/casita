"""Cross physical pack-cache pressure with real S3 latency and bandwidth."""
from __future__ import annotations

import argparse
import itertools
import json
import math
import os
import pathlib
import random
import shutil
import subprocess
import tempfile
import uuid

from benchmarks.lib.tcp_latency_proxy import TcpLatencyProxy
from benchmarks.suites import repository as common
from benchmarks.suites.pack.s3_gc import Rustfs
from benchmarks.suites.scale import fingerprint, nonnegative_csv, positive_csv
from benchmarks.suites.transfer.s3_path import create_rustfs_bucket


def choices_csv(value, allowed):
    values = value.split(",")
    if not values or len(set(values)) != len(values) or not set(values) <= set(allowed):
        raise argparse.ArgumentTypeError(f"expected unique choices from {','.join(allowed)}")
    return values


def build_parser():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--helper", type=pathlib.Path, default=pathlib.Path("target/release/examples/pack_cache_network"))
    parser.add_argument("--baseline-helper", type=pathlib.Path, help="pair each case with an older binary, alternating before/after order")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--cache-mib", type=int)
    parser.add_argument("--working-set-kib", type=positive_csv)
    parser.add_argument("--reads", type=int)
    parser.add_argument("--patterns", type=lambda value: choices_csv(value, ("sequential", "random", "skewed")), default=["sequential", "random", "skewed"])
    parser.add_argument("--phases", type=lambda value: choices_csv(value, ("cold", "warm")), default=["cold", "warm"])
    parser.add_argument("--concurrency", type=positive_csv, default=[1, 8])
    parser.add_argument("--rtt-ms", default="0,25,100")
    parser.add_argument("--bandwidths-kib", default="0,1024,8192", help="per connection, each direction; 0 means unlimited")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path)
    return parser


def dimensions(args):
    cache = args.cache_mib if args.cache_mib is not None else (1 if args.profile == "smoke" else 8)
    working = args.working_set_kib or ([512, 4096] if args.profile == "smoke" else [4096, 32768])
    reads = args.reads if args.reads is not None else (128 if args.profile == "smoke" else 2048)
    if cache < 1 or args.repetitions < 1:
        raise common.BenchmarkError("cache and repetitions must be positive")
    if not min(working) < cache * 1024 < max(working):
        raise common.BenchmarkError("working sets must straddle cache capacity")
    if any(size < 128 or size % 64 for size in working) or reads < max(working) // 64:
        raise common.BenchmarkError("working sets must be multiples of 64 KiB; reads must cover the largest working set")
    return cache, working, reads, nonnegative_csv(args.rtt_ms), nonnegative_csv(args.bandwidths_kib)


def parse_samples(stdout, config):
    samples = [json.loads(line.removeprefix("cache_network_sample ")) for line in stdout.splitlines() if line.startswith("cache_network_sample ")]
    expected = {f"{config['pattern']}-{phase}" for phase in config["phases"]}
    if len(samples) != len(expected) or {sample.get("operation") for sample in samples} != expected:
        raise common.BenchmarkError("missing or duplicate cache/network phases")
    for sample in samples:
        for field in ("wall_seconds", "p50_nanos", "p95_nanos", "p99_nanos", "max_nanos", "backend_read_bytes",
                      "pack_range_requests", "whole_pack_requests", "cache_hits", "cache_evictions", "cache_promotions"):
            value = sample.get(field)
            if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or value < 0:
                raise common.BenchmarkError(f"invalid measurement: {field}")
        if sample.get("status") != "ok" or not sample.get("correctness"):
            raise common.BenchmarkError("probe did not verify reads")
        for field in ("working_set_bytes", "cache_bytes", "concurrency"):
            if sample.get(field) != config[field]:
                raise common.BenchmarkError(f"probe used the wrong {field}")
        if sample.get("operations") != config["reads"] or sample.get("logical_read_bytes") != config["reads"] * 65536:
            raise common.BenchmarkError("probe did not execute all requested reads")
        if not 1 <= sample.get("max_in_flight_reads", 0) <= config["concurrency"]:
            raise common.BenchmarkError("probe exceeded read concurrency")
        if not 0 < sample.get("physical_pack_bytes", 0) or (sample["physical_pack_bytes"] < config["cache_bytes"]) != (config["working_set_bytes"] < config["cache_bytes"]):
            raise common.BenchmarkError("physical fixture does not straddle cache")
        requests = sample["pack_range_requests"] + sample["whole_pack_requests"]
        if sample["operation"].endswith("-cold") and (not requests or not sample["backend_read_bytes"]):
            raise common.BenchmarkError("fresh cold fixture fetched no backend data")
        if sample["operation"].endswith("-warm") and config["working_set_bytes"] < config["cache_bytes"] and (requests or sample["backend_read_bytes"]):
            raise common.BenchmarkError("fitting warm fixture fetched backend data")
        for field in ("fixture_blake3", "access_order_blake3"):
            value = sample.get(field, "")
            if not isinstance(value, str) or len(value) != 64 or any(char not in "0123456789abcdef" for char in value):
                raise common.BenchmarkError(f"invalid fixture identity: {field}")
    return samples


def verify_pair(before, after):
    identity = ("fixture_blake3", "access_order_blake3", "physical_pack_bytes", "pack_count", "pack_target_bytes", "file_bytes")
    left = {sample["operation"]: tuple(sample[field] for field in identity) for sample in before}
    right = {sample["operation"]: tuple(sample[field] for field in identity) for sample in after}
    if left != right:
        raise common.BenchmarkError("before/after fixtures or access orders differ")


def save(args, result):
    common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
    lines = ["# S3 cache-pressure and network benchmark", "", f"Complete: {result['complete']}.", "",
             "Real RustFS through a TCP delay/rate proxy. Rates apply per connection in each direction; concurrency can increase aggregate bandwidth.",
             "Cold uses a fresh cache. Warm uses a fresh handle followed by one complete sequential priming scan. OS caches are not flushed.",
             "Wall time includes byte verification; per-read latency excludes verification and queueing before admission. RSS includes setup.", "",
             "| Variant | Operation | Working KiB | RTT ms | Rate KiB/s | Concurrency | Rep | Wall s | p95 ms | Requests | Backend MiB |",
             "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|"]
    for s in result["samples"]:
        lines.append(f"| {s['variant']} | {s['operation']} | {s['working_set_bytes']//1024} | {s['rtt_ms']} | {s['bandwidth_kib_per_connection']} | {s['concurrency']} | {s['repetition']} | {s['wall_seconds']:.4f} | {s['p95_nanos']/1e6:.3f} | {s['pack_range_requests']+s['whole_pack_requests']} | {s['backend_read_bytes']/2**20:.3f} |")
    if "error" in result:
        lines.extend(["", f"Error: {result['error']}"])
    common.write_atomic(args.report or args.output.with_suffix(".md"), "\n".join(lines) + "\n")


def run(args, work, result):
    cache, working, reads, rtts, rates = dimensions(args)
    if not args.no_build:
        subprocess.run(["cargo", "build", "--release", "--features", "s3,ssh,experimental", "--example", "pack_cache_network", "--locked"], check=True)
    helpers = {"after": args.helper.resolve()}
    if args.baseline_helper:
        helpers["before"] = args.baseline_helper.resolve()
    result["artifacts"] = [{"variant": variant, "path": str(path), "sha256": fingerprint(path)} for variant, path in helpers.items()]
    rustfs_binary = shutil.which("rustfs")
    if rustfs_binary is None:
        raise common.BenchmarkError("rustfs is not available; enter the pinned devenv")
    result["backend_artifact"] = dict(path=rustfs_binary, sha256=fingerprint(pathlib.Path(rustfs_binary)),
        version=subprocess.check_output([rustfs_binary, "--version"], text=True).strip())
    result["configuration"].update(cache_mib=cache, working_set_kib=working, reads=reads, rtt_ms=rtts, bandwidths_kib=rates,
        backend="local RustFS with shaped TCP", rate_scope="per connection per direction", warmup="one full sequential scan on a fresh handle",
        random_pattern="repeated deterministic permutation", max_in_flight_scope="logical payload reads, not backend requests")
    schedule = list(itertools.product(range(1, args.repetitions + 1), working, rtts, rates, args.concurrency, args.patterns))
    random.Random(0xCA517A).shuffle(schedule)
    result["expected_samples"] = len(schedule) * len(helpers) * len(args.phases)
    save(args, result)
    rustfs = Rustfs(work / "rustfs")
    try:
        bucket = f"casita-cache-{uuid.uuid4().hex[:16]}"
        create_rustfs_bucket(rustfs.endpoint, bucket)
        for index, (rep, size, rtt, rate, concurrency, pattern) in enumerate(schedule):
            pair = {}
            variants = ["before", "after"] if args.baseline_helper else ["after"]
            if index % 2:
                variants.reverse()
            for variant in variants:
                print(f"cache/network {index+1}/{len(schedule)}: {variant} rep={rep} working={size}KiB rtt={rtt}ms rate={rate}KiB/s concurrency={concurrency} {pattern}", flush=True)
                with TcpLatencyProxy("127.0.0.1", rustfs.port, rtt) as proxy:
                    proxy.set_bandwidth(rate * 1024)
                    config = dict(backend_endpoint=rustfs.endpoint, read_endpoint=proxy.endpoint, bucket=bucket,
                        prefix=f"case-{index}-{variant}", working_set_bytes=size * 1024, cache_bytes=cache * 2**20,
                        reads=reads, concurrency=concurrency, pattern=pattern, phases=args.phases)
                    stdout, stderr = work / f"{index}-{variant}.stdout", work / f"{index}-{variant}.stderr"
                    timing = common.measured_command(common.CommandSpec([[str(helpers[variant]), json.dumps(config)]], work, dict(os.environ)), stdout, stderr, check=False)
                    captured, errors = stdout.read_text(), stderr.read_text()
                    result["processes"].append(dict(variant=variant, case=config, repetition=rep, rtt_ms=rtt, bandwidth_kib_per_connection=rate, **timing, stdout=captured, stderr=errors))
                    if timing["exit_code"]:
                        raise common.BenchmarkError(f"helper failed ({timing['exit_code']}): {errors}")
                    measured = parse_samples(captured, config)
                    pair[variant] = measured
                    result["samples"].extend(dict(sample, variant=variant, repetition=rep, rtt_ms=rtt,
                        bandwidth_kib_per_connection=rate, process_max_rss_bytes=timing["max_rss_bytes"]) for sample in measured)
                    save(args, result)
            if args.baseline_helper:
                verify_pair(pair["before"], pair["after"])
        if len(result["samples"]) != result["expected_samples"]:
            raise common.BenchmarkError("incomplete cache/network matrix")
    finally:
        rustfs.close()
        with rustfs.log_path.open("rb") as log:
            log.seek(max(0, rustfs.log_path.stat().st_size - 65536))
            result["server_log_tail"] = log.read(65536).decode(errors="replace")


def main(argv=None):
    args = build_parser().parse_args(argv)
    dimensions(args)
    with tempfile.TemporaryDirectory(prefix="casita-cache-network-") as temporary:
        work = pathlib.Path(temporary)
        result = dict(schema_version=1, result_schema="casita.scale.v1", suite_id="huge-repositories",
            environment=common.environment_metadata(work), configuration={key: str(value) if isinstance(value, pathlib.Path) else value for key, value in vars(args).items()},
            complete=False, samples=[], processes=[])
        save(args, result)
        try:
            run(args, work, result)
            result["complete"] = True
        except Exception as error:
            result["error"] = str(error)
            raise
        finally:
            save(args, result)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
