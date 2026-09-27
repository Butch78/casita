"""Direct cached Git pack generation into memory, without any networking."""
from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import selectors
import statistics
import subprocess
import tempfile

from benchmarks.suites import repository as common
from benchmarks.suites.git import git_env
from benchmarks.suites.git_fetch_s3 import fixture, git


MODES = ["serial", "batch-2", "batch-4", "batch-8", "pipeline-8"]


def mode_order(repetition):
    offset = repetition % len(MODES)
    modes = MODES[offset:] + MODES[:offset]
    return modes if repetition // len(MODES) % 2 == 0 else list(reversed(modes))


def event(process):
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ)
        if not selector.select(600):
            raise TimeoutError("direct pack helper did not respond")
    line = process.stdout.readline()
    if not line:
        raise RuntimeError(f"direct pack helper exited: {process.poll()}")
    return json.loads(line)


def validate_pack(pack, identity, work):
    with tempfile.TemporaryDirectory(dir=work, prefix="validate-") as directory:
        client = pathlib.Path(directory) / "client.git"
        git(work, "init", "--bare", "-q", str(client))
        with pack.open("rb") as source:
            result = subprocess.run(["git", "-C", str(client), "index-pack", "--stdin", "--strict"],
                                    stdin=source, capture_output=True, env=git_env(), timeout=600)
        if result.returncode:
            raise RuntimeError(result.stderr.decode(errors="replace"))
        git(client, "update-ref", "refs/heads/main", identity["commit"])
        assert git(client, "rev-parse", "main^{tree}") == identity["tree"]
        assert len(git(client, "rev-list", "--objects", "main").splitlines()) == identity["objects"]
        git(client, "fsck", "--full", "--strict", "--no-progress")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--probe-binary", type=pathlib.Path, required=True)
    parser.add_argument("--nixpkgs", type=pathlib.Path)
    parser.add_argument("--prepared-local-nixpkgs", type=pathlib.Path)
    parser.add_argument("--repetitions", type=int, default=10)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args(argv)
    if args.repetitions < 1:
        parser.error("repetitions must be positive")
    if args.prepared_local_nixpkgs and not args.nixpkgs:
        parser.error("prepared local data requires --nixpkgs")
    binary = args.probe_binary.resolve()
    if not binary.is_file():
        parser.error("build the git_pack_cached example first")
    fingerprint = hashlib.sha256(binary.read_bytes()).hexdigest()
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    artifacts = output.with_name(output.stem + "-artifacts")
    artifacts.mkdir()  # Preserve earlier reports and logs.
    root = pathlib.Path(__file__).resolve().parents[2]
    source_files = ["Cargo.toml", "crates/casita/Cargo.toml", "Cargo.lock", "crates/casita/src/git/fetch/mod.rs", "crates/casita/src/git/fetch/streaming.rs",
                    "crates/casita/src/blob/pack/fetch.rs", "crates/casita/src/blob/chunked.rs", "crates/casita/examples/git_pack_cached.rs",
                    "benchmarks/suites/git_pack_cached.py"]
    report = {"schema_version": 2, "suite_id": "native-git", "benchmark": "git-pack-cached",
              "status": "running", "configuration": {"network": False, "cpu_admission": False,
              "workers": 4, "cache_bytes": 128 * 1024 * 1024, "batches": [1, 2, 4, 8],
              "modes": MODES,
              "repetitions": args.repetitions, "profile": args.profile, "output_sink": "in-process Vec",
              "diagnostics_separate": True}, "binary": {"path": str(binary), "sha256": fingerprint},
              "sources": {f: hashlib.sha256((root / f).read_bytes()).hexdigest() for f in source_files},
              "corpora": {}, "samples": []}

    def save():
        common.write_atomic(output, json.dumps(report, indent=2) + "\n")

    save()
    try:
        with tempfile.TemporaryDirectory(prefix="casita-direct-pack-") as directory:
            work = pathlib.Path(directory)
            for corpus in (["boundary", "nixpkgs"] if args.nixpkgs else ["boundary"]):
                local = work / corpus
                local.mkdir()
                source, identity = fixture(local, args.nixpkgs.resolve() if corpus == "nixpkgs" else None)
                report["corpora"][corpus] = identity
                repository = (args.prepared_local_nixpkgs.resolve()
                              if corpus == "nixpkgs" and args.prepared_local_nixpkgs else local / "repository")
                if not (corpus == "nixpkgs" and args.prepared_local_nixpkgs):
                    with (artifacts / f"{corpus}-prepare.log").open("w") as log:
                        subprocess.run([str(binary), "prepare", str(repository), str(source)],
                                       stdout=log, stderr=log, check=True, timeout=7200)
                pack = local / "result.pack"
                with (artifacts / f"{corpus}-helper.log").open("w") as log:
                    process = subprocess.Popen([str(binary), "run", str(repository), str(pack), identity["commit"]],
                                               stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log,
                                               text=True, bufsize=1)
                    try:
                        ready = event(process)
                        assert ready == {"event": "ready", "objects": identity["objects"]}
                        expected_digest = None
                        plan = [("warmup", -1, "pipeline-8")]
                        plan += [("cached", repetition, mode) for repetition in range(args.repetitions)
                                 for mode in mode_order(repetition)]
                        plan += [("diagnostic", -1, mode) for mode in MODES]
                        for phase, repetition, mode in plan:
                            batch = 1 if mode == "serial" else int(mode.rsplit("-", 1)[1])
                            pipelined = mode.startswith("pipeline-")
                            print(f"{corpus}: {phase} round={repetition} mode={mode}", flush=True)
                            process.stdin.write(json.dumps({"batch": batch, "pipelined": pipelined,
                                                           "diagnostics": phase == "diagnostic"}) + "\n")
                            process.stdin.flush()
                            sample = event(process)
                            assert sample["event"] == "sample" and sample["batch"] == batch
                            assert sample["pipelined"] == pipelined
                            if phase != "warmup":
                                assert sample["payload_requests"] == sample["payload_bytes"] == 0, "timed payloads must be cached"
                            assert sample["diagnostics"] == (phase == "diagnostic")
                            expected_count = identity["objects"] - identity["streaming_objects"] if phase == "diagnostic" else 0
                            assert all(sample[key]["count"] == expected_count for key in ("read", "encode", "handoff"))
                            validate_pack(pack, identity, local)
                            if expected_digest is None:
                                expected_digest = sample["pack_blake3"]
                            assert sample["pack_blake3"] == expected_digest, "all read modes must generate identical pack bytes"
                            report["samples"].append({**sample, "corpus": corpus, "phase": phase,
                                "mode": mode,
                                "repetition": repetition, "wall_seconds": sample["seconds"], "correctness": "passed"})
                            save()
                        process.stdin.close()
                        assert process.wait(timeout=60) == 0
                    finally:
                        if process.poll() is None:
                            process.kill()
                            process.wait()
                        process.stdout.close()
                        if not process.stdin.closed:
                            process.stdin.close()
        assert hashlib.sha256(binary.read_bytes()).hexdigest() == fingerprint
        report["comparisons"] = []
        for corpus in report["corpora"]:
            medians = {mode: statistics.median(s["seconds"] for s in report["samples"]
                       if s["corpus"] == corpus and s["phase"] == "cached" and s["mode"] == mode) for mode in MODES}
            report["comparisons"].append({"corpus": corpus, "median_seconds": medians,
                                          "speedup_over_serial": {mode: medians["serial"] / value for mode, value in medians.items()}})
        report["status"] = "passed"
        save()
        print(json.dumps(report["comparisons"], indent=2), flush=True)
        return 0
    except (Exception, KeyboardInterrupt) as error:
        report["status"] = "failed"
        report["failure"] = str(error) or type(error).__name__
        save()
        raise


if __name__ == "__main__":
    raise SystemExit(main())
