"""Git HTTP fetches from S3/RustFS, with a pinned nixpkgs snapshot option.

Each pair uses one immutable uploaded repository, fresh server processes, and
fresh Git clients. The second fetch reuses the server's payload cache. OS and
RustFS caches are not flushed. No CPU admission or utilization checks apply.
"""
from __future__ import annotations

import argparse
import contextlib
import fcntl
import hashlib
import json
import os
import pathlib
import random
import statistics
import subprocess
import tempfile
import time

from benchmarks.suites import repository as common
from benchmarks.suites.git import git_env
from benchmarks.suites.pack.s3_gc import Rustfs
from benchmarks.suites.transfer.s3_path import create_rustfs_bucket

def git(source, *args, input=None):
    result = subprocess.run(["git", "-C", str(source), *args], input=input,
                            env={**git_env(), "GIT_CONFIG_GLOBAL": "/dev/null",
                                 "GIT_AUTHOR_DATE": "1700000000 +0000", "GIT_COMMITTER_DATE": "1700000000 +0000"},
                            capture_output=True, timeout=3600)
    if result.returncode:
        raise RuntimeError(f"git {args}: {result.stderr.decode(errors='replace')}")
    return result.stdout.decode().strip()


def fixture(work, nixpkgs=None):
    source = work / "source.git"
    git(work, "init", "--bare", "-q", str(source))
    if nixpkgs is not None:
        revision = git(nixpkgs, "rev-parse", "HEAD")
        tree = git(nixpkgs, "rev-parse", f"{revision}^{{tree}}")
        objects = pathlib.Path(git(nixpkgs, "rev-parse", "--path-format=absolute", "--git-path", "objects"))
        (source / "objects/info/alternates").write_text(str(objects) + "\n")
    else:
        revision = None
        lines = []
        rng = random.Random(9)
        for size in (1048575, 1048576, 1048577, 4194304, 16777216):
            for kind in ("text", "random"):
                body = b"a" * size if kind == "text" else rng.randbytes(size)
                oid = git(source, "hash-object", "-w", "--stdin", input=body)
                lines.append(f"100644 blob {oid}\t{kind}-{size}\n")
        tree = git(source, "mktree", input="".join(sorted(lines)).encode())
    # Parentless benchmark commit retains the exact source tree while excluding
    # decades of history. The original clone and refs are never modified.
    commit = git(source, "-c", "user.name=Benchmark", "-c", "user.email=bench@example.invalid",
                 "commit-tree", tree, input=b"S3 fetch benchmark snapshot\n")
    git(source, "update-ref", "refs/heads/main", commit)
    git(source, "symbolic-ref", "HEAD", "refs/heads/main")
    oids = git(source, "rev-list", "--objects", "main").splitlines()
    sizes = git(source, "cat-file", "--batch-check=%(objecttype) %(objectsize)",
                input=("\n".join(line.split()[0] for line in oids) + "\n").encode())
    blobs = [int(line.split()[1]) for line in sizes.splitlines() if line.startswith("blob ")]
    return source, {"revision": revision, "tree": tree, "commit": commit,
                    "objects": len(oids), "blob_bytes": sum(blobs),
                    "blobs_over_1mib": sum(size > 1048576 for size in blobs),
                    "streaming_objects": sum(int(line.split()[1]) > 1048576 for line in sizes.splitlines()),
                    "largest_blob_bytes": max(blobs)}


def phase_stats(previous, current):
    result = {key: current[key] - previous.get(key, 0) for key in (
        "chunk_range_requests", "chunk_range_bytes", "whole_pack_requests",
        "whole_pack_bytes", "cache_hits", "cache_evictions")}
    result["span_totals"] = {name: [value[i] - previous.get("span_totals", {}).get(name, [0, 0])[i]
                                   for i in range(2)] for name, value in current["span_totals"].items()}
    return result


def checked_fixture(cached, identity):
    if cached["identity"] != identity:
        raise ValueError("retained S3 fixture identity differs; select a new --fixture-dir")
    return {**cached["setup"], "reused": True}


def server_samples(binary, identity, prefix, cache, work, environment, diagnostics=False, repeat_fetches=3):
    ready = work / "ready"
    log_path = work / "server.log"
    rows = []
    previous = {}
    with log_path.open("w") as log:
        process = subprocess.Popen([str(binary), "serve", prefix, str(ready), str(cache), "snapshot"],
                                   env=environment, stdin=subprocess.PIPE, stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 600
            while not ready.exists():
                if process.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError(f"server failed to start: {log_path}")
                time.sleep(0.05)
            url = ready.read_text()
            for fetch_index in range(1 + repeat_fetches):
                phase = "first" if fetch_index == 0 else "repeat"
                with tempfile.TemporaryDirectory(prefix="casita-fetch-client-") as client_directory:
                    client = pathlib.Path(client_directory) / "client.git"
                    git(work, "init", "--bare", "-q", str(client))
                    start = time.perf_counter()
                    git(client, "-c", "fetch.fsckObjects=true", "fetch", "--quiet", url,
                        "+refs/heads/main:refs/heads/main")
                    elapsed = time.perf_counter() - start
                    assert git(client, "rev-parse", "main") == identity["commit"]
                    assert git(client, "rev-parse", "main^{tree}") == identity["tree"]
                    assert len(git(client, "rev-list", "--objects", "main").splitlines()) == identity["objects"]
                    git(client, "fsck", "--full", "--strict", "--no-progress")
                    rows.append({"phase": phase, "fetch_index": fetch_index,
                                 "wall_seconds": elapsed, "correctness": "passed"})
                    if diagnostics:
                        process.stdin.write(b"stats\n")
                        process.stdin.flush()
                        deadline = time.monotonic() + 30
                        while True:
                            snapshots = [json.loads(line) for line in log_path.read_text().splitlines()
                                         if line.startswith("{") and '"event":"sample"' in line]
                            if len(snapshots) == len(rows):
                                current = snapshots[-1]
                                rows[-1]["diagnostics"] = phase_stats(previous, current)
                                spans = rows[-1]["diagnostics"]["span_totals"]
                                assert spans["git.fetch.write_pack"][0] == 1, "one timed pack per fetch"
                                assert spans.get("git.fetch.streaming_entry", [0])[0] == identity["streaming_objects"], "all streaming entries must be timed"
                                previous = current
                                break
                            if process.poll() is not None or time.monotonic() > deadline:
                                raise RuntimeError(f"missing per-fetch diagnostics: {log_path}")
                            time.sleep(0.02)
            process.communicate(b"stop\n", timeout=60)
            if process.returncode != 0:
                raise RuntimeError(f"server shutdown failed: {log_path}")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
    stats = [json.loads(line) for line in log_path.read_text().splitlines() if line.startswith("{")][-1]
    assert stats["chunk_range_requests"] + stats["whole_pack_requests"] > 0, "must read backend payloads"
    return rows, stats


def build_parser():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--probe-binary", type=pathlib.Path)
    parser.add_argument("--baseline-binary", type=pathlib.Path)
    parser.add_argument("--nixpkgs", type=pathlib.Path)
    parser.add_argument("--backend", choices=("s3", "local"), default="s3")
    parser.add_argument("--diagnostics", action="store_true")
    parser.add_argument("--fixture-dir", type=pathlib.Path,
                        help="retain and reuse this private RustFS fixture; exclusively locked during the run")
    parser.add_argument("--import-concurrency", type=int, default=1,
                        help="native direct-import concurrency; the source byte budget remains bounded")
    parser.add_argument("--cache-mib", type=int, nargs="+", default=[0, 16, 64, 128])
    parser.add_argument("--fixture-import", choices=("direct", "local-transfer"), default="local-transfer")
    parser.add_argument("--prepared-local-nixpkgs", type=pathlib.Path)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--repetitions", type=int, default=2)
    parser.add_argument("--repeat-fetches", type=int, default=3,
                        help="checked repeat fetches per server after its first fetch (default: 3)")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    return parser


def main(argv=None):
    parser = build_parser()
    args = parser.parse_args(argv)
    if args.repetitions < 1:
        parser.error("repetitions must be positive")
    if args.repeat_fetches < 1:
        parser.error("repeat-fetches must be positive")
    if args.import_concurrency < 1:
        parser.error("import concurrency must be positive")
    if args.fixture_dir and args.backend != "s3":
        parser.error("--fixture-dir applies only to S3; local controls use --prepared-local-nixpkgs")
    if not args.cache_mib or any(value < 0 for value in args.cache_mib) or len(set(args.cache_mib)) != len(args.cache_mib):
        parser.error("cache capacities must be distinct nonnegative MiB values")
    if args.probe_binary is None:
        parser.error("--probe-binary is required (build --release --features s3,git-http,experimental --example git_fetch_s3)")
    if args.prepared_local_nixpkgs and (args.nixpkgs is None or args.fixture_import != "local-transfer"):
        parser.error("prepared local data requires --nixpkgs and --fixture-import local-transfer")
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    artifacts = output.parent / (output.stem + "-artifacts")
    artifacts.mkdir()
    binaries = {"after": args.probe_binary.resolve()}
    if args.baseline_binary:
        binaries = {"before": args.baseline_binary.resolve(), **binaries}
    fingerprints = {name: hashlib.sha256(path.read_bytes()).hexdigest() for name, path in binaries.items()}
    source_root = pathlib.Path(__file__).resolve().parents[2]
    source_paths = ["Cargo.lock", "crates/casita/examples/git_fetch_s3.rs", "crates/casita/src/git/fetch/mod.rs", "crates/casita/src/git/fetch/streaming.rs", "crates/casita/src/metadata/pins/persistent.rs", "crates/casita/src/metadata/pins/runtime.rs", "benchmarks/suites/git_fetch_s3.py", "benchmarks/suites/git_fetch_local.py", "benchmarks/suites/pack/s3_gc.py"]
    report = {"schema_version": 2, "suite_id": "native-git", "benchmark": f"git-fetch-{args.backend}", "status": "running",
              "environment": common.environment_metadata(output.parent),
              "harness_and_candidate_sources": {name: hashlib.sha256((source_root / name).read_bytes()).hexdigest() for name in source_paths},
              "configuration": {"backend": "loopback-rustfs" if args.backend == "s3" else "local", "workers": 4,
                                "diagnostics": args.diagnostics, "cache_mib": args.cache_mib,
                                "fixture_dir": str(args.fixture_dir.resolve()) if args.fixture_dir else None,
                                "import_concurrency": args.import_concurrency,
                                "fixture_import": args.fixture_import,
                                "prepared_local_nixpkgs": str(args.prepared_local_nixpkgs) if args.prepared_local_nixpkgs else None,
                                "git_pack_cache": False, "cpu_admission": False,
                                "repetitions": args.repetitions, "repeat_fetches": args.repeat_fetches,
                                "profile": args.profile},
              "binaries": {name: {"path": str(path), "sha256": fingerprints[name]} for name, path in binaries.items()},
              "corpora": {}, "fixture_setup": {}, "samples": [], "server_runs": []}
    def save():
        common.write_atomic(output, json.dumps(report, indent=2) + "\n")
    save()
    with tempfile.TemporaryDirectory(prefix="casita-git-fetch-s3-") as directory, contextlib.ExitStack() as stack:
        work = pathlib.Path(directory)
        storage = args.fixture_dir.resolve() if args.fixture_dir else work
        storage.mkdir(parents=True, exist_ok=True)
        if args.fixture_dir:
            lock = stack.enter_context((storage / ".lock").open("a+"))
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        manifest_path = storage / "fixtures.json"
        fixtures = json.loads(manifest_path.read_text()) if manifest_path.exists() else {}
        server = Rustfs(storage / "rustfs", reuse=bool(args.fixture_dir)) if args.backend == "s3" else None
        try:
            if server and not manifest_path.exists():
                create_rustfs_bucket(server.endpoint, "casita-git-fetch")
                common.write_atomic(manifest_path, "{}\n")
            environment = {key: value for key, value in os.environ.items() if not key.startswith("AWS_")}
            empty_config = work / "aws-config"
            empty_config.write_text("")
            environment.update(AWS_ACCESS_KEY_ID="minio", AWS_SECRET_ACCESS_KEY="minio123",
                               AWS_REGION="us-east-1", AWS_ENDPOINT_URL=server.endpoint if server else "http://127.0.0.1:1",
                               AWS_ALLOW_HTTP="true", AWS_EC2_METADATA_DISABLED="true",
                               AWS_CONFIG_FILE=str(empty_config), AWS_SHARED_CREDENTIALS_FILE=str(empty_config),
                               NO_PROXY="127.0.0.1,localhost", no_proxy="127.0.0.1,localhost")
            environment["CASITA_BENCH_BACKEND"] = args.backend
            environment["CASITA_BENCH_IMPORT_CONCURRENCY"] = str(args.import_concurrency)
            environment.pop("CASITA_BENCH_DIAGNOSTICS", None)
            if args.diagnostics:
                environment["CASITA_BENCH_DIAGNOSTICS"] = "1"
            for corpus in (["boundary", "nixpkgs"] if args.nixpkgs else ["boundary"]):
                corpus_work = work / corpus
                corpus_work.mkdir()
                source, identity = fixture(corpus_work, args.nixpkgs.resolve() if corpus == "nixpkgs" else None)
                report["corpora"][corpus] = identity
                save()
                print(f"importing {corpus}: {identity}", flush=True)
                prefix = corpus if server else str(corpus_work / "repository")
                environment["CASITA_BENCH_EXPECTED_COMMIT"] = identity["commit"]
                with (artifacts / f"{corpus}-import.log").open("w") as log:
                    import_mode = "import" if args.fixture_import == "direct" else "import-local"
                    if corpus == "nixpkgs" and args.prepared_local_nixpkgs:
                        source = args.prepared_local_nixpkgs.resolve()
                        import_mode = "import-prepared"
                    if server and corpus in fixtures:
                        report["fixture_setup"][corpus] = checked_fixture(fixtures[corpus], identity)
                        print(f"reusing verified {corpus} fixture", flush=True)
                    elif not server and corpus == "nixpkgs" and args.prepared_local_nixpkgs:
                        prefix = str(source)
                    else:
                        start = time.perf_counter()
                        subprocess.run([str(binaries["after"]), import_mode if server else "import", prefix, str(source), "0", "snapshot"],
                                       env=environment, stdout=log, stderr=log, check=True, timeout=7200)
                        setup = {"reused": False, "mode": import_mode if server else "import",
                                 "import_concurrency": args.import_concurrency if import_mode == "import" or not server else None,
                                 "wall_seconds": time.perf_counter() - start}
                        report["fixture_setup"][corpus] = setup
                        if server:
                            fixtures[corpus] = {"identity": identity, "setup": setup}
                            common.write_atomic(manifest_path, json.dumps(fixtures, indent=2) + "\n")
                save()
                for cache in [value * 1024 * 1024 for value in args.cache_mib]:
                    for repetition in range(args.repetitions):
                        order = list(binaries) if repetition % 2 == 0 else list(reversed(binaries))
                        for name in order:
                            label = f"{corpus}-{cache}-{repetition}-{name}"
                            print(f"fetching {label}", flush=True)
                            sample_work = artifacts / label
                            sample_work.mkdir()
                            rows, stats = server_samples(binaries[name], identity, prefix, cache, sample_work, environment,
                                                         args.diagnostics, args.repeat_fetches)
                            report["samples"].extend({**row, "corpus": corpus, "cache_bytes": cache,
                                "repetition": repetition, "implementation": name} for row in rows)
                            report["server_runs"].append({"label": label, "stats": stats})
                            save()
        except (Exception, KeyboardInterrupt) as error:
            report["status"] = "failed"
            report["failure"] = str(error) or type(error).__name__
            save()
            raise
        finally:
            if server:
                server.close()
                (artifacts / "rustfs.log").write_bytes(server.log_path.read_bytes())
    assert fingerprints == {name: hashlib.sha256(path.read_bytes()).hexdigest() for name, path in binaries.items()}
    report["status"] = "passed"
    report["comparisons"] = []
    if "before" in binaries:
        for corpus in report["corpora"]:
            for cache in [value * 1024 * 1024 for value in args.cache_mib]:
                for phase in ("first", "repeat"):
                    medians = {name: statistics.median(row["wall_seconds"] for row in report["samples"]
                        if (row["corpus"], row["cache_bytes"], row["phase"], row["implementation"]) == (corpus, cache, phase, name)) for name in binaries}
                    report["comparisons"].append({"corpus": corpus, "cache_bytes": cache, "phase": phase,
                        "median_seconds": medians, "speedup": medians["before"] / medians["after"]})
    save()
    print(json.dumps(report["comparisons"], indent=2), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
