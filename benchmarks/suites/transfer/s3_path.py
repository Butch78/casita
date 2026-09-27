#!/usr/bin/env python3
"""Measure path-selected transfer depth and request amplification through S3."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import hmac
import json
import os
import pathlib
import random
import socket
import statistics
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from collections.abc import Sequence
from typing import Any

from benchmarks.lib.tcp_latency_proxy import TcpLatencyProxy
from benchmarks.suites import repository as bench
from benchmarks.suites.pack import gc as pack_gc
from benchmarks.suites.pack import s3 as s3_pack
from benchmarks.suites.pack.s3_gc import Rustfs


MIB = 1024 * 1024
REQUIRED_PHASE_METRICS = {
    "wall_nanos",
    "published_objects",
    "payloads_sent",
    "chunks_sent",
    "pack_list_requests",
    "pack_footer_range_requests",
    "pack_footer_range_bytes",
    "pack_chunk_range_requests",
    "pack_chunk_range_bytes",
    "pack_whole_requests",
    "pack_whole_bytes",
    "pack_cache_hits",
    "pack_cache_promotions",
    "wal_writer_open_requests",
    "wal_manifest_load_requests",
    "wal_manifest_refresh_requests",
    "wal_fragment_get_requests",
    "wal_fragment_get_bytes",
    "wal_cache_hits",
    "wal_cache_misses",
}
FASTANT_TSC_PANIC = (
    "fastant-0.1.11/src/tsc_now.rs",
    "attempt to subtract with overflow",
)
TRANSPORTS = ("direct-s3", "atomic-rpc")


def available_tcp_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def _sigv4_key(secret: str, date: str, region: str, service: str) -> bytes:
    def sign(key: bytes, value: str) -> bytes:
        return hmac.new(key, value.encode(), hashlib.sha256).digest()

    date_key = sign(f"AWS4{secret}".encode(), date)
    region_key = sign(date_key, region)
    service_key = sign(region_key, service)
    return sign(service_key, "aws4_request")


def create_rustfs_bucket(endpoint: str, bucket: str) -> None:
    """Create one temporary RustFS bucket without requiring an S3 CLI."""
    parsed = urllib.parse.urlsplit(endpoint)
    if parsed.scheme != "http" or not parsed.netloc:
        raise bench.BenchmarkError(f"invalid local RustFS endpoint: {endpoint}")
    region = "us-east-1"
    payload_hash = hashlib.sha256(b"").hexdigest()
    deadline = time.monotonic() + 10

    def signed_request(method: str) -> urllib.request.Request:
        now = dt.datetime.now(dt.timezone.utc)
        amz_date = now.strftime("%Y%m%dT%H%M%SZ")
        date = now.strftime("%Y%m%d")
        canonical_headers = (
            f"host:{parsed.netloc}\n"
            f"x-amz-content-sha256:{payload_hash}\n"
            f"x-amz-date:{amz_date}\n"
        )
        signed_headers = "host;x-amz-content-sha256;x-amz-date"
        canonical_request = (
            f"{method}\n/{bucket}\n\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
        )
        scope = f"{date}/{region}/s3/aws4_request"
        string_to_sign = (
            f"AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n"
            f"{hashlib.sha256(canonical_request.encode()).hexdigest()}"
        )
        signature = hmac.new(
            _sigv4_key("minio123", date, region, "s3"),
            string_to_sign.encode(),
            hashlib.sha256,
        ).hexdigest()
        authorization = (
            f"AWS4-HMAC-SHA256 Credential=minio/{scope}, "
            f"SignedHeaders={signed_headers}, Signature={signature}"
        )
        headers = {
            "Host": parsed.netloc,
            "x-amz-content-sha256": payload_hash,
            "x-amz-date": amz_date,
            "Authorization": authorization,
        }
        if method == "PUT":
            headers["Content-Length"] = "0"
        return urllib.request.Request(
            f"{endpoint}/{bucket}",
            data=b"" if method == "PUT" else None,
            method=method,
            headers=headers,
        )

    while True:
        try:
            with urllib.request.urlopen(signed_request("PUT"), timeout=2) as response:
                if response.status == 200:
                    break
        except urllib.error.HTTPError as error:
            if error.code != 503 or time.monotonic() >= deadline:
                raise bench.BenchmarkError(
                    f"RustFS bucket creation failed with HTTP {error.code}"
                ) from error
        except OSError as error:
            if time.monotonic() >= deadline:
                raise bench.BenchmarkError("RustFS bucket creation did not become ready") from error
        time.sleep(0.1)

    # RustFS can acknowledge CreateBucket before the bucket is visible to a
    # second client. Wait for the same HeadBucket observation used by S3 SDKs.
    while True:
        try:
            with urllib.request.urlopen(signed_request("HEAD"), timeout=2) as response:
                if response.status == 200:
                    return
        except urllib.error.HTTPError as error:
            if error.code not in {404, 503} or time.monotonic() >= deadline:
                raise bench.BenchmarkError(
                    f"RustFS bucket readiness failed with HTTP {error.code}"
                ) from error
        except OSError as error:
            if time.monotonic() >= deadline:
                raise bench.BenchmarkError("RustFS bucket did not become visible") from error
        time.sleep(0.1)


def non_negative_csv(value: str) -> list[int]:
    try:
        values = [int(item.strip()) for item in value.split(",") if item.strip()]
    except ValueError as error:
        raise argparse.ArgumentTypeError("values must be comma-separated integers") from error
    if not values or any(item < 0 for item in values):
        raise argparse.ArgumentTypeError("values must be non-negative integers")
    if len(values) != len(set(values)):
        raise argparse.ArgumentTypeError("values must not contain duplicates")
    return values


def transport_csv(value: str) -> list[str]:
    transports = [item.strip() for item in value.split(",") if item.strip()]
    if not transports or len(transports) != len(set(transports)):
        raise argparse.ArgumentTypeError("transports must be a non-empty unique list")
    unsupported = set(transports) - set(TRANSPORTS)
    if unsupported:
        raise argparse.ArgumentTypeError(f"unsupported transports: {sorted(unsupported)}")
    return transports


def split_s3_url(value: str) -> tuple[str, str]:
    validated = s3_pack.s3_url(value)
    bucket, separator, prefix = validated.removeprefix("s3://").partition("/")
    if not bucket or not separator or not prefix.strip("/"):
        raise argparse.ArgumentTypeError("expected s3://BUCKET/PREFIX")
    return bucket, prefix.strip("/")


def run_identifier(value: str) -> str:
    if not value or any(
        not (character.isalnum() or character in "-_.") for character in value
    ):
        raise argparse.ArgumentTypeError(
            "run ID must contain only letters, digits, '-', '_', or '.'"
        )
    return value


def interleaved_cases(
    depths: Sequence[int],
    subtree_files: Sequence[int],
    caches: Sequence[int],
    repetitions: int,
    rtts: Sequence[int] = (0,),
    transports: Sequence[str] = ("direct-s3",),
) -> list[tuple[str, int, int, int, int, int]]:
    """Return deterministic per-repetition shuffles of the complete matrix."""
    configurations = [
        (transport, rtt, depth, files, cache)
        for transport in transports
        for rtt in rtts
        for depth in depths
        for files in subtree_files
        for cache in caches
    ]
    randomizer = random.Random(0xCA517A)
    scheduled = []
    for repetition in range(1, repetitions + 1):
        block = configurations.copy()
        randomizer.shuffle(block)
        scheduled.extend((*configuration, repetition) for configuration in block)
    return scheduled


def parse_metrics(
    stdout: str,
    expected_wal_refreshes: int = 1,
) -> dict[str, dict[str, int]]:
    phases: dict[str, dict[str, int]] = {"cold": {}, "warm": {}}
    for line in stdout.splitlines():
        parts = line.split()
        if len(parts) != 2:
            continue
        name, value = parts
        phase, separator, metric = name.partition("-")
        if not separator or phase not in phases:
            continue
        try:
            phases[phase][metric.replace("-", "_")] = int(value)
        except ValueError:
            continue
    for phase, metrics in phases.items():
        missing = REQUIRED_PHASE_METRICS - metrics.keys()
        if missing:
            raise bench.BenchmarkError(f"{phase} helper metrics are missing {sorted(missing)}")
        if metrics["pack_list_requests"] != 0 or metrics["pack_footer_range_requests"] != 0:
            raise bench.BenchmarkError(
                f"{phase} transfer unexpectedly rebuilt pack inventory/catalog state"
            )
        if metrics["wal_fragment_get_requests"] != 0:
            raise bench.BenchmarkError(
                f"{phase} transfer reloaded wal3 fragments after the source opened"
            )
        if metrics["wal_manifest_refresh_requests"] != expected_wal_refreshes:
            raise bench.BenchmarkError(
                f"{phase} transfer used {metrics['wal_manifest_refresh_requests']} "
                f"wal3 manifest refreshes instead of {expected_wal_refreshes}"
            )
    return phases


def run_helper(
    command: list[str],
    attempts: int = 3,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    """Retry only fastant's intermittent pre-main TSC calibration abort."""
    for attempt in range(1, attempts + 1):
        completed = subprocess.run(
            command,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=env,
        )
        fastant_tsc_abort = all(fragment in completed.stderr for fragment in FASTANT_TSC_PANIC)
        if completed.returncode == 0 or not fastant_tsc_abort or attempt == attempts:
            return completed
        time.sleep(0.05)
    raise AssertionError("helper retry loop did not return")


def check_rpc_budget(phases: dict[str, dict[str, int]], maximum: int) -> None:
    for phase, metrics in phases.items():
        requests = metrics.get("rpc_requests")
        if requests is None or not 0 < requests <= maximum:
            raise bench.BenchmarkError(
                f"{phase} RPC requests {requests} violate the 1..{maximum} command budget"
            )


def run_helper_measured(
    command: list[str],
    workspace: pathlib.Path,
    label: str,
    attempts: int = 3,
    env: dict[str, str] | None = None,
) -> tuple[subprocess.CompletedProcess[str], dict[str, object]]:
    """Run a helper with the fastant retry policy and capture peak process RSS."""
    helper_env = dict(os.environ) if env is None else env
    stdout = workspace / f"{label}.stdout"
    stderr = workspace / f"{label}.stderr"
    for attempt in range(1, attempts + 1):
        timing = bench.measured_command(
            bench.CommandSpec([command], workspace, helper_env),
            stdout,
            stderr,
            check=False,
        )
        completed = subprocess.CompletedProcess(
            command,
            int(timing["exit_code"]),
            stdout.read_text(errors="replace"),
            stderr.read_text(errors="replace"),
        )
        fastant_tsc_abort = all(fragment in completed.stderr for fragment in FASTANT_TSC_PANIC)
        if completed.returncode == 0 or not fastant_tsc_abort or attempt == attempts:
            return completed, timing
        time.sleep(0.05)
    raise AssertionError("helper retry loop did not return")


def deterministic_bytes(label: str, size: int) -> bytes:
    output = bytearray()
    counter = 0
    while len(output) < size:
        output.extend(hashlib.sha256(f"{label}:{counter}".encode()).digest())
        counter += 1
    return bytes(output[:size])


def generate_tree(root: pathlib.Path, depth: int, subtree_files: int, file_bytes: int) -> str:
    root.mkdir(parents=True)
    current = root
    components: list[str] = []
    for level in range(depth):
        (current / f"sibling-{level:04d}.bin").write_bytes(
            deterministic_bytes(f"sibling-{depth}-{level}", file_bytes)
        )
        component = f"level-{level:04d}"
        components.append(component)
        current = current / component
        current.mkdir()
    selected = current / "selected"
    selected.mkdir()
    for index in range(subtree_files):
        (selected / f"file-{index:06d}.bin").write_bytes(
            deterministic_bytes(f"selected-{depth}-{subtree_files}-{index}", file_bytes)
        )
    components.append("selected")
    return "/".join(components)


def render_report(result: dict[str, Any]) -> str:
    transports = {
        str(sample.get("transport", "direct-s3")) for sample in result["samples"]
    }
    grouped: dict[tuple[str, int, int, int, int, str], list[dict[str, int]]] = {}
    for sample in result["samples"]:
        for phase in ("cold", "warm"):
            grouped.setdefault(
                (
                    str(sample.get("transport", "direct-s3")),
                    int(sample.get("rtt_ms", 0)),
                    int(sample["depth"]),
                    int(sample["subtree_files"]),
                    int(sample["cache_mib"]),
                    phase,
                ),
                [],
            ).append(sample[phase])
    lines = [
        "# Casita S3 path-selected transfer benchmark",
        "",
        f"Endpoint: `{result['configuration']['endpoint']}`. Latency label: "
        f"`{result['configuration']['latency_label']}`.",
        "",
        "The benchmark uses the real S3/wal3 repository profile. Import, source open, and destination validation are outside each timed transfer.",
        "",
        "| Transport | RTT | Depth | Subtree files | Cache | Phase | n | Median | p95 | Range GETs | Whole GETs | Cache hits | Source bytes | wal3 refreshes |",
        "|---|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for (
        transport,
        rtt_ms,
        depth,
        subtree_files,
        cache_mib,
        phase,
    ), samples in sorted(grouped.items()):
        walls = [sample["wall_nanos"] / 1_000_000_000 for sample in samples]

        def median(name: str) -> float:
            return statistics.median(sample[name] for sample in samples)

        source_bytes = median("pack_chunk_range_bytes") + median("pack_whole_bytes")
        lines.append(
            f"| {transport} | {rtt_ms} ms | {depth} | {subtree_files} | {cache_mib} MiB | "
            f"{phase} | {len(samples)} | "
            f"{statistics.median(walls):.4f} s | {bench.percentile(walls, 0.95):.4f} s | "
            f"{median('pack_chunk_range_requests'):g} | {median('pack_whole_requests'):g} | "
            f"{median('pack_cache_hits'):g} | {bench.human_bytes(int(source_bytes))} | "
            f"{median('wal_manifest_refresh_requests'):g} |"
        )
    resource_samples = [
        sample for sample in result["samples"] if "process_resources" in sample
    ]
    if resource_samples:
        lines.extend(
            [
                "",
                "## Helper process resources",
                "",
                "Peak RSS covers the helper process that performs both cold and warm transfer phases.",
                "",
                "| Transport | RTT | Depth | Subtree files | Cache | n | Median process wall | Median peak RSS |",
                "|---|---:|---:|---:|---:|---:|---:|---:|",
            ]
        )
        resource_grouped: dict[tuple[str, int, int, int, int], list[dict[str, object]]] = {}
        for sample in resource_samples:
            key = (
                str(sample.get("transport", "direct-s3")),
                int(sample.get("rtt_ms", 0)),
                int(sample["depth"]),
                int(sample["subtree_files"]),
                int(sample["cache_mib"]),
            )
            resource_grouped.setdefault(key, []).append(sample["process_resources"])
        for (transport, rtt_ms, depth, subtree_files, cache_mib), samples in sorted(
            resource_grouped.items()
        ):
            lines.append(
                f"| {transport} | {rtt_ms} ms | {depth} | {subtree_files} | "
                f"{cache_mib} MiB | {len(samples)} | "
                f"{statistics.median(float(sample['wall_seconds']) for sample in samples):.4f} s | "
                f"{bench.human_bytes(int(statistics.median(float(sample['max_rss_bytes']) for sample in samples)))} |"
            )
    sensitivity: dict[tuple[str, int, int, int, str], list[tuple[int, float]]] = {}
    for (
        transport,
        rtt_ms,
        depth,
        subtree_files,
        cache_mib,
        phase,
    ), samples in grouped.items():
        sensitivity.setdefault((transport, depth, subtree_files, cache_mib, phase), []).append(
            (rtt_ms, statistics.median(sample["wall_nanos"] / 1_000_000 for sample in samples))
        )
    if any(len(points) > 1 for points in sensitivity.values()):
        lines.extend(
            [
                "",
                "## RTT sensitivity",
                "",
                "The fitted milliseconds-per-millisecond slope estimates serialized network turns. It should be read beside the exact request ledger above.",
                "",
                "| Transport | Depth | Subtree files | Cache | Phase | Fitted turns | 0 ms median | Max RTT median |",
                "|---|---:|---:|---:|---|---:|---:|---:|",
            ]
        )
        for (transport, depth, subtree_files, cache_mib, phase), unsorted in sorted(
            sensitivity.items()
        ):
            points = sorted(unsorted)
            if len(points) < 2:
                continue
            mean_rtt = statistics.mean(rtt for rtt, _ in points)
            mean_wall = statistics.mean(wall for _, wall in points)
            slope = sum(
                (rtt - mean_rtt) * (wall - mean_wall) for rtt, wall in points
            ) / sum((rtt - mean_rtt) ** 2 for rtt, _ in points)
            baseline = next((wall for rtt, wall in points if rtt == 0), points[0][1])
            maximum_rtt, maximum_wall = points[-1]
            lines.append(
                f"| {transport} | {depth} | {subtree_files} | {cache_mib} MiB | {phase} | "
                f"{slope:.2f} | {baseline:.1f} ms | {maximum_wall:.1f} ms "
                f"at {maximum_rtt} ms |"
            )
    lines.extend([""])
    if "atomic-rpc" in transports:
        lines.extend(
            [
                "Depth counts directory edges above the selected directory. With direct S3, each extra uncached level is causally dependent on decoding its parent. With atomic RPC, those depth-dependent S3 reads happen beside the repository while the client observes one bounded proof response.",
                "",
            ]
        )
    else:
        lines.extend(
            [
                "Depth counts directory edges above the selected directory. Each extra uncached level is causally dependent on decoding its parent. Independent selected-subtree payload reads can overlap only after resolution.",
                "",
            ]
        )
    lines.extend(
        [
            "The warm phase reuses only the source handle and its pack cache. It always transfers into a fresh verified destination, so destination deduplication cannot hide source reads.",
            "",
        ]
    )
    if result.get("remote_prefixes"):
        lines.extend(
            [
                "Deployed-endpoint timings are comparable only within the same endpoint, runner location, latency label, and run window. Treat fewer than 10 repetitions as a smoke test.",
                "",
                "Remote benchmark prefixes are intentionally retained. Their exact URLs are recorded in the raw JSON for cleanup under the bucket owner policy.",
                "",
            ]
        )
    elif result["configuration"].get("latency_mode") in {
        "controlled-rustfs",
        "controlled-rustfs-links",
    }:
        controlled_note = (
            "Configured RTT is applied either to the direct S3 link or to the client-to-resolver link by rootless relays. Resolver-to-RustFS traffic remains on loopback; repository import and source open remain outside the timer."
            if "atomic-rpc" in transports
            else "Configured RTT is applied to the direct S3 link by a rootless relay. Repository import and source open remain outside the timer."
        )
        lines.extend([controlled_note, ""])
    else:
        lines.extend(
            [
                "Loopback RustFS timings establish request shape, not production network latency. A deployed-endpoint run should use at least 10 repetitions before selecting an RPC or path-index design.",
                "",
            ]
        )
    return "\n".join(lines)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--depths", default="0,4,16")
    parser.add_argument("--subtree-files", default="1,64")
    parser.add_argument("--cache-mib", default="0,64")
    parser.add_argument("--bandwidth-kib", type=int, default=0, help="per-connection KiB/s in each direction; 0 is unlimited")
    parser.add_argument("--max-rpc-requests", type=int, help="require each RPC phase to stay within this command count")
    parser.add_argument("--file-kib", type=int, default=4)
    parser.add_argument("--pack-target-mib", type=int, default=4)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument(
        "--rtt-ms",
        help="comma-separated controlled RustFS RTTs; enables the rootless TCP proxy",
    )
    parser.add_argument(
        "--transports",
        default="direct-s3",
        help="comma-separated transport matrix: direct-s3,atomic-rpc",
    )
    parser.add_argument(
        "--s3-url",
        type=split_s3_url,
        metavar="s3://BUCKET/PREFIX",
        help="use a caller-owned deployed S3 prefix instead of temporary RustFS",
    )
    parser.add_argument("--latency-label")
    parser.add_argument("--run-id", type=run_identifier)
    parser.add_argument(
        "--helper", type=pathlib.Path, default=pathlib.Path("target/release/examples/s3_path_transfer")
    )
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument(
        "--require-clean",
        action="store_true",
        help="fail unless the Casita worktree is clean before collecting samples",
    )
    parser.add_argument("--keep-work", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--report", type=pathlib.Path)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    depths = non_negative_csv(args.depths)
    subtree_files = pack_gc.unique_positive_csv(args.subtree_files)
    caches = non_negative_csv(args.cache_mib)
    transports = transport_csv(args.transports)
    if args.bandwidth_kib < 0:
        raise SystemExit("bandwidth must be non-negative")
    if args.max_rpc_requests is not None and args.max_rpc_requests < 1:
        raise SystemExit("RPC command budget must be positive")
    controlled_latency = args.rtt_ms is not None or args.bandwidth_kib > 0
    rtts = non_negative_csv(args.rtt_ms) if args.rtt_ms is not None else [0]
    if controlled_latency and args.s3_url:
        raise SystemExit("--rtt-ms controls temporary RustFS and cannot be combined with --s3-url")
    if "atomic-rpc" in transports and args.s3_url:
        raise SystemExit("--transports atomic-rpc requires local RustFS")
    if min(args.file_kib, args.pack_target_mib, args.repetitions) < 1:
        raise SystemExit(
            "--file-kib, --pack-target-mib, and --repetitions must be positive"
        )
    if not args.no_build:
        subprocess.run(
            [
                "cargo",
                "build",
                "--release",
                "--example",
                "s3_path_transfer",
                "--features",
                "s3,ssh,experimental",
            ],
            check=True,
        )
    args.helper = args.helper.resolve()
    if not args.helper.exists():
        raise SystemExit(f"S3 path benchmark helper does not exist: {args.helper}")

    temporary = None
    if args.keep_work is None:
        temporary = tempfile.TemporaryDirectory(prefix="casita-s3-path-")
        work = pathlib.Path(temporary.name)
    else:
        work = args.keep_work.resolve()
        work.mkdir(parents=True, exist_ok=True)
    environment = bench.environment_metadata(work)
    if args.require_clean and environment["casita_worktree_dirty"]:
        raise bench.BenchmarkError("publication run requires a clean Casita worktree")
    timestamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    run_id = args.run_id or f"{timestamp}-{uuid.uuid4().hex[:8]}"
    output = args.output or pathlib.Path("benchmarks/results") / f"s3-path-transfer-{timestamp}.json"
    report = args.report or output.with_suffix(".md")
    rustfs_backend_port = available_tcp_port() if controlled_latency else 9000
    rustfs_console_port = available_tcp_port() if controlled_latency else 9001
    while rustfs_console_port == rustfs_backend_port:
        rustfs_console_port = available_tcp_port()
    rustfs = (
        None
        if args.s3_url
        else Rustfs(work / "rustfs", rustfs_backend_port, rustfs_console_port)
    )
    endpoint = (
        f"s3://{args.s3_url[0]}/{args.s3_url[1]}"
        if args.s3_url
        else "rustfs+controlled-links://loopback"
        if controlled_latency
        else "rustfs://127.0.0.1:9000"
    )
    latency_mode = (
        "endpoint-native"
        if args.s3_url
        else "controlled-rustfs-links"
        if controlled_latency
        else "loopback-rustfs"
    )
    latency_label = args.latency_label or latency_mode
    trees = {}
    for depth in depths:
        for files in subtree_files:
            source_tree = work / f"tree-depth-{depth}-files-{files}"
            trees[(depth, files)] = (
                source_tree,
                generate_tree(source_tree, depth, files, args.file_kib * 1024),
            )
    schedule = interleaved_cases(
        depths, subtree_files, caches, args.repetitions, rtts, transports
    )
    samples: list[dict[str, Any]] = []
    remote_prefixes: list[str] = []
    latency_proxy = None
    rustfs_bucket = None
    try:
        if controlled_latency:
            latency_proxy = TcpLatencyProxy(
                "127.0.0.1",
                rustfs_backend_port,
                0,
            )
            rustfs_bucket = f"casita-path-{uuid.uuid4().hex[:16]}"
            create_rustfs_bucket(
                f"http://127.0.0.1:{rustfs_backend_port}",
                rustfs_bucket,
            )
        for sample_index, (
            transport,
            rtt_ms,
            depth,
            files,
            cache_mib,
            repetition,
        ) in enumerate(
            schedule, start=1
        ):
            source_tree, selected_path = trees[(depth, files)]
            print(
                f"[{sample_index}/{len(schedule)}] transport={transport} "
                f"rtt={rtt_ms}ms depth={depth} "
                f"files={files} cache={cache_mib}MiB repetition={repetition}",
                flush=True,
            )
            writer = (
                f"path-{transport}-{rtt_ms}-{depth}-{files}-{cache_mib}-"
                f"{repetition}-{time.time_ns()}"
            )
            helper_arguments = [
                str(source_tree),
                selected_path,
                str(args.pack_target_mib * MIB),
                str(cache_mib * MIB),
                writer,
            ]
            source_url = None
            helper_env = None
            if args.s3_url:
                bucket, base_prefix = args.s3_url
                sample_prefix = (
                    f"{base_prefix}/{run_id}/depth-{depth}-files-{files}/"
                    f"cache-{cache_mib}/repetition-{repetition}-{sample_index}"
                )
                source_url = f"s3://{bucket}/{sample_prefix}"
                remote_prefixes.append(source_url)
                helper_arguments = ["--s3", bucket, sample_prefix, *helper_arguments]
            elif controlled_latency:
                assert latency_proxy is not None
                assert rustfs_bucket is not None
                latency_proxy.set_rtt_ms(rtt_ms if transport == "direct-s3" else 0)
                latency_proxy.set_bandwidth(args.bandwidth_kib * 1024 if transport == "direct-s3" else 0)
                backend_endpoint = f"http://127.0.0.1:{rustfs_backend_port}"
                source_endpoint = (
                    latency_proxy.endpoint if transport == "direct-s3" else backend_endpoint
                )
                sample_prefix = (
                    f"path-transfer/{run_id}/{transport}/rtt-{rtt_ms}/"
                    f"depth-{depth}-files-{files}/"
                    f"cache-{cache_mib}/repetition-{repetition}-{sample_index}"
                )
                helper_arguments = [
                    "--s3",
                    rustfs_bucket,
                    sample_prefix,
                    *helper_arguments,
                ]
                helper_env = {
                    **os.environ,
                    "AWS_ACCESS_KEY_ID": "minio",
                    "AWS_SECRET_ACCESS_KEY": "minio123",
                    "AWS_REGION": "us-east-1",
                    "AWS_DEFAULT_REGION": "us-east-1",
                    "AWS_ENDPOINT": source_endpoint,
                    "AWS_ENDPOINT_URL": source_endpoint,
                    "AWS_ENDPOINT_URL_S3": source_endpoint,
                    "AWS_ALLOW_HTTP": "true",
                    "AWS_EC2_METADATA_DISABLED": "true",
                }
                helper_env.pop("AWS_SESSION_TOKEN", None)
                helper_env.pop("AWS_PROFILE", None)
            if transport == "atomic-rpc":
                helper_arguments = [
                    "--resolver-rtt-ms",
                    str(rtt_ms if controlled_latency else 0),
                    *helper_arguments,
                ]
            helper_env = {**(helper_env or os.environ), "CASITA_BENCH_BANDWIDTH_BYTES_PER_SECOND": str(args.bandwidth_kib * 1024)}
            completed, process_metrics = run_helper_measured(
                [str(args.helper), *helper_arguments],
                work,
                f"helper-{sample_index}",
                env=helper_env,
            )
            if completed.returncode != 0:
                retained = f"\nremote prefix: {source_url}" if source_url else ""
                raise bench.BenchmarkError(
                    f"S3 path helper failed ({completed.returncode})\n"
                    f"stdout:\n{completed.stdout}\nstderr:\n{completed.stderr}{retained}"
                )
            phases = parse_metrics(
                completed.stdout,
                expected_wal_refreshes=0 if transport == "atomic-rpc" else 1,
            )
            if transport == "atomic-rpc" and args.max_rpc_requests is not None:
                check_rpc_budget(phases, args.max_rpc_requests)
            sample = {
                "transport": transport,
                "rtt_ms": rtt_ms,
                "bandwidth_kib_per_connection": args.bandwidth_kib,
                "depth": depth,
                "subtree_files": files,
                "cache_mib": cache_mib,
                "repetition": repetition,
                "execution_index": sample_index,
                "selected_path": selected_path,
                "process_resources": {
                    "wall_seconds": process_metrics["wall_seconds"],
                    "user_seconds": process_metrics["user_seconds"],
                    "system_seconds": process_metrics["system_seconds"],
                    "max_rss_bytes": process_metrics["max_rss_bytes"],
                },
                **phases,
            }
            if source_url:
                sample["source_url"] = source_url
            samples.append(sample)
        result: dict[str, Any] = {
            "schema_version": 1,
            "result_schema": "casita.s3-path-transfer.v4",
            "suite_id": "transfer",
            "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
            "environment": environment,
            "configuration": {
                "endpoint": endpoint,
                "latency_label": latency_label,
                "latency_mode": latency_mode,
                "run_id": run_id,
                "execution_order": "seeded-interleaved-v1",
                "depths": depths,
                "subtree_files": subtree_files,
                "cache_mib": caches,
                "rtt_ms": rtts,
                "bandwidth_kib_per_connection": args.bandwidth_kib,
                "max_rpc_requests": args.max_rpc_requests,
                "transports": transports,
                "file_kib": args.file_kib,
                "pack_target_mib": args.pack_target_mib,
                "repetitions": args.repetitions,
            },
            "remote_prefixes": remote_prefixes,
            "samples": samples,
        }
        bench.write_atomic(output, json.dumps(result, indent=2, sort_keys=True) + "\n")
        bench.write_atomic(report, render_report(result))
        print(f"result: {output}")
        print(f"report: {report}")
    finally:
        if latency_proxy is not None:
            latency_proxy.close()
        if rustfs is not None:
            rustfs.close()
        if temporary is not None:
            temporary.cleanup()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
