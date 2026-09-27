"""Full repository fsck under sustained spill and across the metadata-cache cutoff."""
from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import os
import pathlib
import random
import re
import shutil
import subprocess
import tempfile

from benchmarks.suites import graph
from benchmarks.suites import repository as common
from benchmarks.suites.metadata_collection import positive_csv


PROFILES = {"smoke": [256], "standard": [8192, 65536], "frontier": [249755]}
STEADY_LIMITS = {"smoke": [32, 128], "standard": [1024, 4096], "frontier": [1024, 4096]}
CORRECTNESS = "healthy fsck, exact root/object/payload counts and revision, unchanged root and checkout"
SEED_PROBE = "repository::fsck_benchmark::benchmark_seed_fsck_fixture"


def build_parser():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--casita", default=shutil.which("casita") or "casita")
    parser.add_argument("--baseline-casita", help="alternate a second immutable CLI on each identical fixture")
    parser.add_argument("--seed-probe", type=pathlib.Path,
                        help="library test executable for untimed exclusive fixture construction")
    parser.add_argument("--profile", choices=PROFILES, default="standard")
    parser.add_argument("--files", type=positive_csv)
    parser.add_argument("--memory-objects", type=positive_csv,
                        help="explicit memory limits; defaults to sustained-spill limits plus N-1, N, N+1")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--spill-bytes", type=int, default=4 * 1024**3)
    workspace = parser.add_mutually_exclusive_group()
    workspace.add_argument("--work-dir", type=pathlib.Path,
                           help="create and retain a new fixture directory for follow-up profiling")
    workspace.add_argument("--reuse-work-dir", type=pathlib.Path,
                           help="reuse retained fixtures; repeat all correctness audits")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    return parser


def parse_fsck(stdout, objects, revision=None):
    # A healthy fixture produces exactly these three lines. Reject issues,
    # missing/duplicate summaries, and partial output even if the CLI exits 0.
    match = re.fullmatch(
        r"revision (\S+); checked (\d+) root\(s\), (\d+) object\(s\), (\d+) payload\(s\)\n"
        r"traversal-spill-files-opened (\d+); traversal-spill-peak-bytes (\d+)\n"
        r"logical-fsck-nanos (\d+)\n?", stdout,
    )
    if match is None:
        raise common.BenchmarkError("fsck output is incomplete or contains unexpected issues")
    actual_revision, roots, records, payloads, files, peak, nanos = match.groups()
    if (int(roots), int(records), int(payloads)) != (1, objects, objects):
        raise common.BenchmarkError("fsck did not check the exact fixture inventory")
    if revision is not None and actual_revision != revision:
        raise common.BenchmarkError("fsck changed the snapshot revision")
    return {"revision": actual_revision, "roots_checked": int(roots),
            "objects_checked": int(records), "payloads_checked": int(payloads),
            "spill_files_opened": int(files), "spill_peak_bytes": int(peak),
            "logical_fsck_seconds": int(nanos) / 1e9}


def checked(arguments, env=None):
    result = subprocess.run(arguments, capture_output=True, text=True, env=env)
    if result.returncode:
        raise common.BenchmarkError(f"command failed ({result.returncode}): {arguments}\n{result.stdout}\n{result.stderr}")
    return result.stdout


def parse_seed(stdout, files, objects):
    try:
        cases = [json.loads(line.removeprefix("fsck_fixture ")) for line in stdout.splitlines()
                 if line.startswith("fsck_fixture ")]
    except json.JSONDecodeError as error:
        raise common.BenchmarkError("invalid fixture probe JSON") from error
    if ("test result: ok. 1 passed; 0 failed;" not in stdout or len(cases) != 1
            or not isinstance(cases[0], dict)
            or cases[0].get("files") != files or cases[0].get("objects") != objects):
        raise common.BenchmarkError("fixture probe must pass and report the exact fixture size")
    return cases[0]


def fsck_command(executable, repository, memory_objects, spill_bytes):
    return graph.command(executable, repository, "--log-filter", "off",
                         "--spill-memory-objects", str(memory_objects),
                         "--spill-bytes", str(spill_bytes), "fsck", "--audit-only")


def audit_checkout(executable, repository, target, destination, manifest):
    with tempfile.TemporaryDirectory(prefix="checkout-audit-", dir=destination.parent) as temporary:
        checkout = pathlib.Path(temporary) / "tree"
        checked(graph.command(executable, repository, "checkout", target, str(checkout), "--no-root"))
        common.assert_manifest(checkout, manifest)


def save(args, result):
    common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")


def memory_limits(args, objects):
    if args.memory_objects is not None:
        return args.memory_objects.copy()
    return sorted({*STEADY_LIMITS[args.profile], objects - 1, objects, objects + 1})


@contextlib.contextmanager
def workspace_directory(path, reuse=False):
    if path is None:
        with tempfile.TemporaryDirectory(prefix="casita-fsck-") as temporary:
            yield pathlib.Path(temporary)
    else:
        path = path.resolve()
        if reuse:
            if not path.is_dir():
                raise common.BenchmarkError("retained fixture directory does not exist")
        else:
            path.mkdir(parents=True, exist_ok=False)
        yield path


def run_fixture(args, result, work, files):
    # Each unique file contributes one blob; each occupied branch and the
    # top-level directory contribute one directory record/payload. The exact
    # count is derived from the fixture, not inferred from the measured fsck.
    branches = max(64, (files + 1023) // 1024)
    objects = files + min(files, branches) + 1
    workspace = work / str(files)
    repository = workspace / "repository"
    seed = None
    if not args.reuse_work_dir:
        if args.seed_probe:
            graph.source_tree(workspace / "retained", files, "retained", branches=branches)
            invocation = [str(args.seed_probe), SEED_PROBE, "--exact", "--ignored", "--nocapture"]
            env = {**os.environ, "CASITA_BENCH_FSCK_REPOSITORY": str(repository),
                   "CASITA_BENCH_FSCK_FILES": str(files)}
            stdout = checked(invocation, env=env)
            seed = {"command": invocation, "stdout": stdout,
                    "case": parse_seed(stdout, files, objects)}
        else:
            graph.setup(args.casita, workspace, files, branches=branches)
    manifest = common.tree_manifest(workspace / "retained")
    roots_command = graph.command(args.casita, repository, "root", "ls")
    roots = checked(roots_command)
    if len(roots.splitlines()) != 1 or roots.split()[1:] != ["bench/retained"]:
        raise common.BenchmarkError("fixture must have exactly the retained root")
    target = roots.split()[0]
    audit_checkout(args.casita, repository, target, workspace / "before", manifest)
    audit_command = fsck_command(args.casita, repository, objects + 1, args.spill_bytes)
    audit_stdout = checked(audit_command)
    audit = parse_fsck(audit_stdout, objects)
    fixture = {"files": files, "branches": min(files, branches), "objects": objects, "root": roots.strip(),
               "manifest_sha256": common.manifest_identity(manifest),
               "audit_command": audit_command, "audit_stdout": audit_stdout,
               "checkout_before": "ok", "checkout_after": "pending", "seed": seed}
    result["fixtures"].append(fixture)
    save(args, result)
    for repetition in range(1, args.repetitions + 1):
        thresholds = memory_limits(args, objects)
        random.Random(files + repetition).shuffle(thresholds)
        variants = args.artifacts if repetition % 2 else list(reversed(args.artifacts))
        for threshold, artifact in [(threshold, artifact) for threshold in thresholds for artifact in variants]:
            offset = threshold - objects
            invocation = fsck_command(artifact["path"], repository, threshold, args.spill_bytes)
            sample = {"status": "failed", "implementation": "casita", "operation": "fsck",
                      "variant": artifact["variant"], "binary_sha256": artifact["sha256"],
                      "entries": objects, "files": files, "spill_memory_objects": threshold,
                      "cache_mode": "streaming" if offset < 0 else "cached",
                      "limit_offset": offset, "repetition": repetition,
                      "cache_policy": "warm", "command": invocation}
            result["samples"].append(sample)
            stdout, stderr = workspace / "timed.stdout", workspace / "timed.stderr"
            print(f"fsck {artifact['variant']} files={files} records={objects} memory={threshold} repetition={repetition}", flush=True)
            try:
                sample.update(common.measured_command(
                    common.CommandSpec(steps=[invocation], cwd=workspace,
                                       env={**os.environ, "LC_ALL": "C", "TZ": "UTC"}),
                    stdout, stderr, check=False))
                sample.update(stdout=stdout.read_text(), stderr=stderr.read_text())
                if sample["exit_code"]:
                    raise common.BenchmarkError(f"fsck exited {sample['exit_code']}: {sample['stderr']}")
                metrics = parse_fsck(sample["stdout"], objects, audit["revision"])
                # Metadata caching accepts exactly N records, whereas SpillSet
                # moves to disk as soon as it reaches N entries. The at-limit
                # case therefore keeps the record cache AND spills traversal sets.
                if offset <= 0 and not metrics["spill_files_opened"]:
                    raise common.BenchmarkError("the at/below-inventory limit did not exercise spill")
                if offset > 0 and metrics["spill_files_opened"]:
                    raise common.BenchmarkError("the above-inventory limit unexpectedly spilled")
                if graph.active_spill_files(repository):
                    raise common.BenchmarkError("fsck left active spill files behind")
                sample.update(status="ok", metrics=metrics,
                              correctness="healthy fsck, exact counts and revision, spill expectation and cleanup")
            except (common.BenchmarkError, OSError) as error:
                sample["error"] = str(error)
                raise
            finally:
                save(args, result)
    if checked(roots_command) != roots:
        raise common.BenchmarkError("fsck changed the retained root")
    audit_checkout(args.casita, repository, target, workspace / "after", manifest)
    fixture["checkout_after"] = "ok"
    fixture["correctness"] = CORRECTNESS
    save(args, result)


def main(argv=None):
    parser = build_parser()
    args = parser.parse_args(argv)
    if args.repetitions < 1 or args.spill_bytes < 1:
        parser.error("repetitions and spill bytes must be positive")
    args.artifacts = []
    for variant, requested in (("baseline", args.baseline_casita), ("candidate", args.casita)):
        if requested is None:
            continue
        executable = shutil.which(requested)
        if executable is None:
            parser.error(f"Casita executable not found: {requested}")
        path = str(pathlib.Path(executable).resolve())
        with open(path, "rb") as handle:
            digest = hashlib.file_digest(handle, "sha256").hexdigest()
        args.artifacts.append({"variant": variant, "path": path, "sha256": digest})
    if len({artifact["sha256"] for artifact in args.artifacts}) != len(args.artifacts):
        parser.error("baseline and candidate binaries must have distinct hashes")
    args.casita = args.artifacts[-1]["path"]
    seed_artifact = None
    if args.seed_probe:
        args.seed_probe = args.seed_probe.resolve()
        with args.seed_probe.open("rb") as handle:
            seed_artifact = {"path": str(args.seed_probe),
                             "sha256": hashlib.file_digest(handle, "sha256").hexdigest()}
    files = args.files or PROFILES[args.profile]
    result = {"schema_version": 1, "result_schema": "casita.fsck.v1",
              "suite_id": "collection-and-fsck", "complete": False,
              "configuration": {"profile": args.profile, "files": files,
                                "limit_offsets": [-1, 0, 1] if args.memory_objects is None else None,
                                "steady_spill_limits": STEADY_LIMITS[args.profile] if args.memory_objects is None else [],
                                "memory_objects": args.memory_objects, "repetitions": args.repetitions,
                                "spill_bytes": args.spill_bytes, "mode": "audit-only"},
              "artifacts": args.artifacts, "fixture_builder": seed_artifact,
              "fixtures": [], "samples": [],
              "notes": "Each timed fsck is a fresh CLI process after setup audits warmed the repository. Wall time includes startup/open/teardown; logical_fsck_seconds measures the API call. Peak RSS excludes import and checkout. Limits apply per traversal structure, not to total process memory."}
    with workspace_directory(args.reuse_work_dir or args.work_dir, bool(args.reuse_work_dir)) as work:
        result["configuration"]["work_dir"] = str(work)
        result["configuration"]["reused_fixture"] = bool(args.reuse_work_dir)
        result["environment"] = common.environment_metadata(work)
        save(args, result)
        try:
            for count in files:
                run_fixture(args, result, work, count)
            for artifact in [*args.artifacts, *([seed_artifact] if seed_artifact else [])]:
                with open(artifact["path"], "rb") as handle:
                    if hashlib.file_digest(handle, "sha256").hexdigest() != artifact["sha256"]:
                        raise common.BenchmarkError("Casita executable changed during measurement")
            result["complete"] = True
        except (common.BenchmarkError, RuntimeError, OSError) as error:
            result["error"] = str(error)
            print(f"fsck benchmark failed: {error}", flush=True)
        finally:
            save(args, result)
    print(f"wrote {args.output}")
    return 0 if result["complete"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
