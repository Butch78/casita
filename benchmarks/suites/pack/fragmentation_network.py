"""Production-sized packs and edited build artifacts over latency-shaped S3."""
from __future__ import annotations

import argparse
import contextlib
import itertools
import json
import os
import pathlib
import random
import shutil
import subprocess
import tempfile
import time
import urllib.request
import uuid

from benchmarks import cli
from benchmarks.lib.redact import local_paths
from benchmarks.lib.tcp_latency_proxy import TcpLatencyProxy
from benchmarks.suites import repository as common
from benchmarks.suites.pack.catalog import parse_probe_binary
from benchmarks.suites.pack.s3_gc import Rustfs
from benchmarks.suites.scale import fingerprint, nonnegative_csv
from benchmarks.suites.transfer.s3_path import create_rustfs_bucket

PROBE = "blob::pack::fragmentation_network::benchmark_s3_fragmentation"
CARGO_ARGUMENTS = ("test", "--release", "--features", "cli,s3", "--lib", "--no-run", "--message-format=json")
CACHES = ("disabled", "below-largest-pack", "above-largest-pack", "default", "below-working-set", "ample", "below-live-chunks", "live-chunks", "above-live-chunks")
PREPARED = "all historical versions and fresh control reconstructed and independently hashed"
CORRECTNESS = "exact bytes and independent BLAKE3; ample warm cache has zero pack GETs"


def positive(value):
    if type(value) is not int or value <= 0:
        raise common.BenchmarkError("expected positive integer metric")


def rows(stdout, prefix):
    if "test result: ok. 1 passed; 0 failed;" not in stdout:
        raise common.BenchmarkError("S3 fragmentation Rust gate did not pass")
    return [json.loads(line.removeprefix(prefix)) for line in stdout.splitlines() if line.startswith(prefix)]


def parse_prepared(stdout, config):
    samples = rows(stdout, "s3_fragmentation_prepared ")
    if len(samples) != 1:
        raise common.BenchmarkError("expected exactly one prepared fixture")
    sample = samples[0]
    if (sample.get("correctness") != PREPARED or sample.get("generations") != config["generations"]
            or sample.get("pack_target_bytes") != 16 * 2**20 or sample.get("default_cache_bytes") != 64 * 2**20
            or sample.get("avg_chunk_bytes") != 256 * 1024):
        raise common.BenchmarkError("wrong fixture or production defaults")
    for layout in ("history", "fresh"):
        item = sample[layout]
        for field in ("file_bytes", "chunks", "referenced_packs", "pack_runs", "largest_pack_bytes",
                      "referenced_pack_bytes", "stored_bytes", "stored_pack_bytes"):
            positive(item[field])
        if item["prefix"] != config["prefix"] + "/" + layout:
            raise common.BenchmarkError("wrong fixture storage prefix")
        if not item["referenced_packs"] <= item["pack_runs"] <= item["chunks"]:
            raise common.BenchmarkError("invalid pack topology")
        if "read_plan" in item:
            from benchmarks.suites.pack.read_planning import validate_trace
            try:
                validate_trace(item)
            except (ValueError, KeyError, TypeError) as error:
                raise common.BenchmarkError(f"invalid physical read plan: {error}") from error
    if any(sample["history"][k] != sample["fresh"][k] for k in ("blob", "file_bytes", "chunks")):
        raise common.BenchmarkError("fresh and historical layouts differ logically")
    if all("read_plan" in sample[layout] for layout in ("history", "fresh")):
        identities = [[(c["digest"], c["size"]) for c in sample[layout]["read_plan"]]
                      for layout in ("history", "fresh")]
        if identities[0] != identities[1]:
            raise common.BenchmarkError("physical plans disagree on logical chunk order")
    if not sample["history"]["blob"] or len(sample.get("original_blake3", "")) != 64:
        raise common.BenchmarkError("missing content identity")
    if [s["generation"] for s in sample["writes"]] != list(range(config["generations"] + 1)):
        raise common.BenchmarkError("incomplete write history")
    return sample


def cache_capacity(prepared, label):
    # Both layouts receive the same capacity. Ample includes each layout's
    # complete referenced pack set, not all retained historical packs.
    largest = max(prepared[layout]["largest_pack_bytes"] for layout in ("history", "fresh"))
    ample = max(prepared[layout]["referenced_pack_bytes"] for layout in ("history", "fresh"))
    if label in ("below-live-chunks", "live-chunks", "above-live-chunks"):
        live = max(sum({chunk["digest"]: chunk["framed_len"] for chunk in prepared[layout]["read_plan"]}.values())
                   for layout in ("history", "fresh"))
        return live + {"below-live-chunks": -1, "live-chunks": 0, "above-live-chunks": 1}[label]
    return dict(zip(CACHES[:6], (0, largest - 1, largest + 1, prepared["default_cache_bytes"], ample - 1, ample)))[label]


def parse_samples(stdout, config):
    samples = rows(stdout, "s3_fragmentation_sample ")
    if len(samples) != 2 or {s.get("phase") for s in samples} != {"cold", "warm"}:
        raise common.BenchmarkError("missing or duplicate read phases")
    descriptor = config["prepared"][config["layout"]]
    partial = 0 < config.get("read_bytes", 0) < descriptor["file_bytes"]
    for sample in samples:
        if sample.get("correctness") != CORRECTNESS:
            raise common.BenchmarkError("unverified reconstruction")
        for field in ("layout", "cache", "cache_bytes"):
            if sample.get(field) != config[field]:
                raise common.BenchmarkError("wrong read configuration")
        for field in ("blob", "file_bytes"):
            if sample.get(field) != descriptor[field]:
                raise common.BenchmarkError("wrong reconstructed content")
        positive(sample.get("nanos"))
        if config.get("require_stream_metrics"):
            positive(sample.get("first_byte_nanos"))
            if (sample["first_byte_nanos"] > sample["nanos"]
                    or sample.get("read_strategy") != config["read_strategy"]
                    or sample.get("read_bytes") != config["read_bytes"]):
                raise common.BenchmarkError("wrong stream timing or read strategy")
            if config["read_strategy"] in {"planned", "pipeline", "lookahead"} and (
                    type(sample.get("response_peak_bytes")) is not int
                    or not 0 <= sample["response_peak_bytes"] <= 64 * 2**20):
                raise common.BenchmarkError("planned response buffers exceeded the bound")
        positive(sample.get("reopen_nanos"))
        for field in ("pack_requests", "pack_read_bytes", "whole_pack_requests", "chunk_range_requests",
                      "cache_hits", "cache_promotions", "cache_evictions"):
            if type(sample.get(field)) is not int or sample[field] < 0:
                raise common.BenchmarkError("invalid read counter")
        for counters in (sample["origin"], sample["reopen_origin"]):
            if set(counters) != {"gets", "get_bytes", "heads", "puts", "put_bytes", "lists"}:
                raise common.BenchmarkError("incomplete origin accounting")
            if any(type(value) is not int or value < 0 for value in counters.values()):
                raise common.BenchmarkError("invalid origin counter")
            if counters["puts"] or counters["put_bytes"]:
                raise common.BenchmarkError("read phase mutated origin")
        if ((not partial and sample["origin"]["gets"] < sample["pack_requests"])
                or sample["origin"]["get_bytes"] < sample["pack_read_bytes"]):
            raise common.BenchmarkError("origin undercounts pack reads")
        if "pack_io" in sample:
            packs = sample["pack_io"]
            if not isinstance(packs, list):
                raise common.BenchmarkError("invalid per-pack I/O trace")
            paths = set()
            for pack in packs:
                if (not isinstance(pack, dict) or not isinstance(pack.get("path"), str) or "/packs/" not in pack["path"]
                        or pack["path"] in paths
                        or any(type(pack.get(k)) is not int or pack[k] < 0
                               for k in ("gets", "bytes", "whole_gets"))
                        or pack["whole_gets"] > pack["gets"]):
                    raise common.BenchmarkError("invalid per-pack I/O trace")
                paths.add(pack["path"])
            gets = sum(p["gets"] for p in packs)
            declared_bytes = sum(p["bytes"] for p in packs)
            whole_gets = sum(p["whole_gets"] for p in packs)
            # A cancelled prefetch can have started without receiving headers,
            # or received headers without finishing its body. Keep the three
            # ledgers distinct for partial reads; full reads must match exactly.
            mismatch = (gets > sample["pack_requests"]
                        or declared_bytes < sample["pack_read_bytes"]
                        or whole_gets > sample["whole_pack_requests"]
                        or gets > sample["origin"]["gets"]
                        or declared_bytes > sample["origin"]["get_bytes"])
            if not partial:
                mismatch |= (gets != sample["pack_requests"]
                             or declared_bytes != sample["pack_read_bytes"]
                             or whole_gets != sample["whole_pack_requests"])
            if mismatch:
                raise common.BenchmarkError("per-pack trace disagrees with read counters")
        if sample["phase"] == "cold" and not (sample["pack_requests"] and sample["pack_read_bytes"]):
            raise common.BenchmarkError("cold cache performed no pack I/O")
        if config["cache_bytes"] == 0 and sample["whole_pack_requests"]:
            raise common.BenchmarkError("disabled cache promoted a pack")
        if (not partial and sample["phase"] == "warm" and config["cache_bytes"] >= descriptor["referenced_pack_bytes"]
                and (sample["pack_requests"] or sample["pack_read_bytes"])):
            raise common.BenchmarkError("ample warm cache performed pack I/O")
        if sample.get("production_fetch") == "planned-chunk-cache-v1":
            positive(config["live_chunk_bytes"])
            if (not partial and sample["phase"] == "warm" and config["cache_bytes"] >= config["live_chunk_bytes"]
                    and (sample["pack_requests"] or sample["pack_read_bytes"])):
                raise common.BenchmarkError("fitting compressed-chunk cache performed pack I/O")
    return samples


def build_parser():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--probe-binary", type=pathlib.Path)
    parser.add_argument("--baseline-probe-binary", type=pathlib.Path,
                        help="alternate baseline/candidate reads against the same prepared objects")
    parser.add_argument("--include-pipeline-control", action="store_true", help="add the demand-driven pipeline using the same binary")
    parser.add_argument("--compare-current", action="store_true", help="use the same probe's current reader as baseline")
    parser.add_argument("--candidate-read-strategy", choices=("current", "planned", "pipeline", "lookahead"), default="current")
    parser.add_argument("--window-mib", type=int, choices=(1, 4, 16), default=16)
    parser.add_argument("--read-bytes", default="0", help="comma-separated read limits; 0 means whole file")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--artifact", type=pathlib.Path, help="real release executable; standard defaults to the probe binary")
    parser.add_argument("--rtt-ms", default="0,20,80")
    parser.add_argument("--caches", nargs="+", choices=CACHES, default=list(CACHES),
                        help="cache cases to run; defaults to the complete boundary matrix")
    parser.add_argument("--layouts", nargs="+", choices=("history", "fresh"), default=["history", "fresh"],
                        help="layouts to measure; preparation still audits the complete history")
    parser.add_argument("--proxy-buffer-kib", type=int, default=65536,
                        help="per-connection, per-direction queue budget; 512 reproduces the older small-buffer control")
    parser.add_argument("--include-small-buffer-control", action="store_true",
                        help="add one ample-cache 80 ms pair per corpus using the legacy 512 KiB proxy queue")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--output", type=pathlib.Path, default=cli.ROOT / "benchmarks/results/s3-fragmentation.json")
    parser.add_argument("--report", type=pathlib.Path)
    return parser


def save(args, result):
    common.write_atomic(args.output, json.dumps(local_paths(result), indent=2) + "\n")
    lines = ["# Fragmentation over S3", "", f"Complete: {result['complete']}", "",
             "Real RustFS, fixed added TCP propagation delay, unlimited bandwidth. Production 16 MiB pack target.",
             "Controlled 4 KiB edits to deterministic bytes or real build-output bytes; not a real rebuild history.",
             "Warm follows two reads of the same requested scope. Reopen and independent byte/hash checks are outside read timing.",
             "For cancelled partial reads, pack counters count requests started/completed bodies; origin counters count returned headers/declared body sizes, not wire bytes.", "",
             "| Variant | Read bytes | Corpus | RTT ms | Proxy KiB | Cache | Layout | Phase | Rep | ms | First byte ms | Pack GETs | Pack MiB | Origin GETs | Origin MiB |",
             "|---|---:|---|---:|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|"]
    for s in result["samples"]:
        lines.append(f"| {s.get('variant', 'single')} | {s.get('read_bytes', 0) or 'all'} | {s['corpus']} | {s['rtt_ms']} | {s['proxy_buffer_bytes']//1024} | {s['cache']} | {s['layout']} | {s['phase']} | {s['repetition']} | "
                     f"{s['nanos']/1e6:.3f} | {s['first_byte_nanos']/1e6 if 'first_byte_nanos' in s else ''} | {s['pack_requests']} | {s['pack_read_bytes']/2**20:.3f} | "
                     f"{s['origin']['gets']} | {s['origin']['get_bytes']/2**20:.3f} |")
    if "error" in result:
        lines += ["", result["error"]]
    common.write_atomic(args.report or args.output.with_suffix(".md"), "\n".join(lines) + "\n")


def invoke(binary, config, work, result):
    index = len(result["processes"])
    stdout, stderr = work / f"{index}.stdout", work / f"{index}.stderr"
    environment = {**os.environ, "CASITA_S3_FRAGMENTATION": json.dumps(config)}
    timing = common.measured_command(common.CommandSpec(
        [[str(binary), PROBE, "--exact", "--ignored", "--nocapture"]], work, environment), stdout, stderr, check=False)
    captured, errors = stdout.read_text(), stderr.read_text()
    result["processes"].append(dict(config=config, **timing, stdout=captured, stderr=errors))
    if timing["exit_code"]:
        raise common.BenchmarkError(f"fragmentation probe failed: {errors}")
    return captured


def run(args, work, result):
    binary = args.probe_binary
    if binary is None:
        built = subprocess.run(["cargo", *CARGO_ARGUMENTS], cwd=cli.ROOT, capture_output=True, text=True)
        if built.returncode:
            raise common.BenchmarkError(built.stderr or built.stdout)
        binary = parse_probe_binary(built.stdout)
    binary = binary.resolve()
    variants = {"candidate": binary}
    if args.baseline_probe_binary:
        variants["baseline"] = args.baseline_probe_binary.resolve()
    elif args.compare_current:
        variants["baseline"] = binary
    if args.include_pipeline_control:
        variants["pipeline-control"] = binary
    result["artifacts"] = [dict(variant=variant, path=str(path), sha256=fingerprint(path))
                           for variant, path in variants.items()]
    rtts = nonnegative_csv(args.rtt_ms)
    corpus_configs = {"random": dict(file_bytes=(8 if args.profile == "smoke" else 64) * 2**20)}
    if args.profile == "standard" or args.artifact:
        artifact = (args.artifact or binary).resolve()
        if artifact.stat().st_size < 1024 * 1024:
            raise common.BenchmarkError("artifact must contain at least 1 MiB")
        # Freeze the artifact against concurrent builds replacing its path.
        frozen = work / "release-executable"
        shutil.copyfile(artifact, frozen)
        result["corpus_artifact"] = dict(path=str(artifact), sha256=fingerprint(frozen), bytes=frozen.stat().st_size)
        corpus_configs["release-executable"] = dict(artifact=str(frozen))
    generations = 4 if args.profile == "smoke" else 32
    result["configuration"].update(generations=generations, corpus_configs=corpus_configs,
        caches=args.caches, layouts=args.layouts, variants=list(variants), rtt_ms=rtts, bandwidth="unlimited", read_ahead_chunks=2,
        proxy_buffer_bytes=args.proxy_buffer_kib * 1024,
        include_small_buffer_control=args.include_small_buffer_control,
        candidate_read_strategy=args.candidate_read_strategy, window_bytes=args.window_mib * 2**20,
        read_bytes=nonnegative_csv(args.read_bytes),
        fixture_edits="4096 bytes in a deterministic permutation of 32 disjoint regions", chunk_upload_concurrency=1)
    rustfs_path = pathlib.Path(shutil.which("rustfs") or "missing-rustfs")
    result["backend_artifact"] = dict(path=str(rustfs_path), sha256=fingerprint(rustfs_path),
        version=subprocess.check_output([str(rustfs_path), "--version"], text=True).strip())
    server = Rustfs(work / "rustfs")
    try:
        bucket = "casita-fragmentation-" + uuid.uuid4().hex[:12]
        create_rustfs_bucket(server.endpoint, bucket)
        configs = {}
        for name, corpus_config in corpus_configs.items():
            print(f"preparing {name}: {generations} edits and full historical audit", flush=True)
            config = dict(corpus_config, mode="prepare", endpoint=server.endpoint, bucket=bucket,
                          prefix=name, generations=generations)
            prepared = parse_prepared(invoke(binary, config, work, result), config)
            configs[name] = config
            result["fixtures"][name] = prepared
            save(args, result)
        with contextlib.ExitStack() as stack:
            schedules = [(name, rtt, cache, rep, args.proxy_buffer_kib * 1024, limit)
                         for name, rtt, cache, rep, limit in itertools.product(corpus_configs, rtts, args.caches, range(1, args.repetitions + 1), nonnegative_csv(args.read_bytes))]
            if args.include_small_buffer_control:
                schedules += [(name, 80, "ample", 1, 512 * 1024, 0) for name in corpus_configs]
            proxy_keys = sorted({(rtt, buffer) for _, rtt, _, _, buffer, _ in schedules})
            proxies = {(rtt, buffer): stack.enter_context(TcpLatencyProxy("127.0.0.1", server.port, rtt,
                        buffer_bytes=buffer)) for rtt, buffer in proxy_keys}
            for (rtt, buffer), proxy in proxies.items():
                calibration = []
                for _ in range(3):
                    start = time.monotonic()
                    with urllib.request.urlopen(proxy.endpoint + "/minio/health/ready", timeout=10) as response:
                        assert response.status == 200
                        response.read()
                    calibration.append((time.monotonic() - start) * 1000)
                if min(calibration) < rtt * 0.9:
                    raise common.BenchmarkError("proxy did not apply the configured propagation delay")
                result["calibration"].append(dict(rtt_ms=rtt, proxy_buffer_bytes=buffer, observed_http_ms=calibration))
            random.Random(0xCA517A).shuffle(schedules)
            result["expected_samples"] = len(schedules) * 2 * len(args.layouts) * len(variants)
            save(args, result)
            for index, (corpus_name, rtt, cache, repetition, buffer, limit) in enumerate(schedules):
                prepared = result["fixtures"][corpus_name]
                layouts = ("history", "fresh") if index % 2 == 0 else ("fresh", "history")
                layouts = [layout for layout in layouts if layout in args.layouts]
                print(f"read pair {index+1}/{len(schedules)}: {corpus_name} rtt={rtt} cache={cache} rep={repetition} proxy-buffer={buffer}", flush=True)
                for layout in layouts:
                    config = dict(configs[corpus_name], mode="read", endpoint=proxies[rtt, buffer].endpoint,
                        # The exported trace is preparation evidence, not read
                        # input; keep it out of the subprocess environment.
                        prepared={key: {field: value for field, value in prepared[key].items()
                                        if field != "read_plan"} for key in ("history", "fresh")},
                        layout=layout, cache=cache, cache_bytes=cache_capacity(prepared, cache),
                        live_chunk_bytes=(sum({chunk["digest"]: chunk["framed_len"] for chunk in prepared[layout]["read_plan"]}.values())
                                          if "read_plan" in prepared[layout] else None))
                    order = (list(itertools.permutations(variants))[index % 6] if len(variants) == 3
                             else list(variants) if index % 2 == 0 else list(reversed(variants)))
                    for variant in order:
                        strategy = (args.candidate_read_strategy if variant == "candidate"
                                    else "pipeline" if variant == "pipeline-control" else "current")
                        variant_config = dict(config, variant=variant, read_bytes=limit,
                            window_bytes=args.window_mib * 2**20,
                            require_stream_metrics=strategy != "current" or args.candidate_read_strategy != "current" or limit > 0,
                            read_strategy=strategy)
                        for sample in parse_samples(invoke(variants[variant], variant_config, work, result), variant_config):
                            result["samples"].append(dict(sample, variant=variant, corpus=corpus_name,
                                rtt_ms=rtt, repetition=repetition, proxy_buffer_bytes=buffer))
                        save(args, result)
            if len(result["samples"]) != result["expected_samples"]:
                raise common.BenchmarkError("incomplete network matrix")
    finally:
        server.close()
        result["server_log_tail"] = server.log_path.read_bytes()[-65536:].decode(errors="replace")


def main(argv=None):
    args = build_parser().parse_args(argv)
    if args.repetitions < 1 or (args.no_build and args.probe_binary is None):
        raise common.BenchmarkError("positive repetitions and a binary for --no-build are required")
    if len(set(args.caches)) != len(args.caches):
        raise common.BenchmarkError("cache cases must be unique")
    if len(set(args.layouts)) != len(args.layouts):
        raise common.BenchmarkError("layouts must be unique")
    if args.proxy_buffer_kib < 64 or args.proxy_buffer_kib % 64:
        raise common.BenchmarkError("proxy buffer must be a positive multiple of 64 KiB")
    nonnegative_csv(args.rtt_ms)
    if args.include_small_buffer_control and (80 not in nonnegative_csv(args.rtt_ms) or args.proxy_buffer_kib == 512):
        raise common.BenchmarkError("small-buffer control requires 80 ms and a different primary buffer")
    with tempfile.TemporaryDirectory(prefix="casita-s3-fragmentation-") as directory:
        result = dict(schema_version=1, result_schema="casita.s3-fragmentation.v1", suite_id="blob-backends",
            environment=common.environment_metadata(cli.ROOT), configuration=dict(profile=args.profile, repetitions=args.repetitions),
            complete=False, samples=[], fixtures={}, processes=[], calibration=[])
        save(args, result)
        try:
            run(args, pathlib.Path(directory), result)
            result["complete"] = True
        except Exception as error:
            result["error"] = str(error)
            raise
        finally:
            save(args, result)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
