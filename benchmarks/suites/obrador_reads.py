"""Obrador's existing shared-DAG NAR/read_file workload, with concurrent physical GC."""
from __future__ import annotations

import argparse
import contextlib
import hashlib
import itertools
import json
import math
import os
import pathlib
import re
import shutil
import subprocess

from benchmarks import cli
from benchmarks.suites import repository as common
from benchmarks.suites.metadata_collection import positive_csv

CORRECTNESS = "exact payloads, registered paths and shared-DAG references before and after GC"


def use_durable_admission(source):
    """Select the durable control in either repository source layout."""
    package = source / "crates/casita" if (source / "crates/casita/Cargo.toml").exists() else source
    repository = package / "src/repository/retention.rs"
    if not repository.exists():
        repository = package / "src/repository.rs"
    code = repository.read_text()
    marker = "self.owned_retention_hold_kind(true).await"
    if code.count(marker) != 1:
        raise common.BenchmarkError("cannot locate retained-reader admission control")
    repository.write_text(code.replace(marker, "self.owned_retention_hold_kind(false).await"))


def checked(command, **kwargs):
    return subprocess.check_output(command, **kwargs)


def snapshot(source, destination):
    """Copy current tracked contents, including dirty edits, without changing the source."""
    source = source.resolve()
    revision = checked(["git", "rev-parse", "HEAD"], cwd=source).decode().strip()
    patch = checked(["git", "diff", "HEAD", "--binary"], cwd=source)
    paths = checked(["git", "ls-files", "-z"], cwd=source).decode().split("\0")
    if (source / "Cargo.lock").exists() and "Cargo.lock" not in paths:
        paths.append("Cargo.lock")
    files = {}
    for relative in sorted(set(paths) - {""}):
        original = source / relative
        if not original.exists() and not original.is_symlink():
            continue
        target = destination / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(original, target, follow_symlinks=False)
        files[relative] = hashlib.sha256(target.read_bytes()).hexdigest()
    if patch != checked(["git", "diff", "HEAD", "--binary"], cwd=source):
        raise common.BenchmarkError("source changed during snapshot")
    return {"source": str(source), "revision": revision,
            "patch_sha256": hashlib.sha256(patch).hexdigest(), "files": files}


def parse_sample(stdout, paths, workers, iterations, gc):
    rows = [json.loads(line) for line in stdout.splitlines() if line.startswith("{")]
    if len(rows) != 1:
        raise common.BenchmarkError("Obrador probe must emit one verified case")
    row = rows[0]
    if any(row.get(key) != value for key, value in dict(paths=paths, workers=workers,
            iterations=iterations, concurrent_gc=gc, correctness=CORRECTNESS).items()):
        raise common.BenchmarkError("wrong Obrador configuration or missing correctness gates")
    reads, collections = row.get("read_nanos"), row.get("gc_nanos")
    if (not isinstance(reads, list) or len(reads) != workers * iterations
            or not isinstance(collections, list) or (gc and not collections)
            or (not gc and collections) or any(type(n) is not int or n < 0 for n in reads + collections)):
        raise common.BenchmarkError("incomplete Obrador read/GC samples")
    ordered = sorted(reads)
    for p in (50, 95, 99):
        if row.get(f"p{p}_nanos") != ordered[(len(reads) * p + 99) // 100 - 1]:
            raise common.BenchmarkError("incorrect Obrador percentile")
    if (not isinstance(row.get("wall_seconds"), (int, float))
            or not math.isfinite(row["wall_seconds"]) or row["wall_seconds"] <= 0):
        raise common.BenchmarkError("invalid Obrador elapsed time")
    return row


def save(output, result):
    common.write_atomic(output, json.dumps(result, indent=2) + "\n")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--obrador-source", type=pathlib.Path, default=os.environ.get("CASITA_OBRADOR_SOURCE"))
    parser.add_argument("--casita-source", type=pathlib.Path, default=cli.ROOT)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="smoke")
    parser.add_argument("--paths", type=positive_csv)
    parser.add_argument("--workers", type=positive_csv, default=[1, 8])
    parser.add_argument("--iterations", type=int)
    parser.add_argument("--repetitions", type=int, default=1)
    parser.add_argument("--timeout", type=int, default=300)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--work-dir", type=pathlib.Path)
    parser.add_argument("--reuse-build-report", type=pathlib.Path,
                        help="reuse hash-verified binaries and source provenance from a completed run")
    parser.add_argument("--require-quiet-host", action="store_true",
                        help="Linux: wait for competing jobs to finish and reject contaminated samples")
    parser.add_argument("--quiet-timeout", type=int, default=900)
    parser.add_argument("--rebuild-probe", action="store_true",
                        help="rebuild the current probe in the retained copied workspaces")
    parser.add_argument("--rebuild-casita", action="store_true",
                        help="snapshot --casita-source into retained workspaces, preserving Obrador")
    parser.add_argument("--perf", type=pathlib.Path, help="perf executable for scoped read-phase CPU profiles")
    args = parser.parse_args(argv)
    if args.obrador_source is None and args.reuse_build_report is None:
        parser.error("--obrador-source or CASITA_OBRADOR_SOURCE is required")
    if (args.rebuild_probe or args.rebuild_casita) and args.reuse_build_report is None:
        parser.error("rebuilding requires --reuse-build-report")
    paths = args.paths or ([12] if args.profile == "smoke" else [12, 100])
    iterations = args.iterations if args.iterations is not None else (20 if args.profile == "smoke" else 200)
    if min(iterations, args.repetitions, args.timeout, args.quiet_timeout) < 1:
        parser.error("counts and timeout must be positive")
    output = args.output.resolve()
    work = (args.work_dir or output.with_suffix(".work")).resolve()
    work.mkdir(parents=True, exist_ok=False)
    result = {"schema_version": 1, "result_schema": "casita.obrador-reads.v1",
              "suite_id": "repository-e2e", "complete": False,
              "environment": common.environment_metadata(work), "configuration": {
                  "profile": args.profile, "paths": paths, "workers": args.workers,
                  "iterations": iterations, "repetitions": args.repetitions,
                  "require_quiet_host": args.require_quiet_host,
                  "perf": str(args.perf) if args.perf else None,
                  "control": "same Casita source with only owned_read_hold admission set to durable"},
              "samples": [], "work_directory": str(work)}
    save(output, result)
    try:
        if args.reuse_build_report:
            previous = json.loads(args.reuse_build_report.read_text())
            if previous.get("result_schema") != "casita.obrador-reads.v1" or not previous.get("complete"):
                raise common.BenchmarkError("reuse requires a completed Obrador report")
            binaries = previous["binaries"]
            if set(binaries) != {"durable", "process"}:
                raise common.BenchmarkError("reuse requires both protection variants")
            for entry in binaries.values():
                if hashlib.sha256(pathlib.Path(entry["path"]).read_bytes()).hexdigest() != entry["sha256"]:
                    raise common.BenchmarkError("reused benchmark binary changed")
            result["sources"] = previous["sources"]
            result["probe_sha256"] = previous.get("probe_sha256")
            result["build_environment"] = previous.get("build_environment", previous["environment"])
            result["reused_build_report"] = str(args.reuse_build_report.resolve())
            if args.rebuild_probe or args.rebuild_casita:
                from benchmarks.obrador_profile import rebuild
                casita_source = None
                if args.rebuild_casita:
                    casita_source = work / 'sources/casita'
                    result['sources'] = {**previous['sources'],
                        'casita': snapshot(args.casita_source, casita_source)}
                binaries, result["build_workspaces"] = rebuild(previous, work, casita_source)
                result["probe_sha256"] = hashlib.sha256((cli.ROOT / "benchmarks/obrador_reads.rs").read_bytes()).hexdigest()
                result["build_environment"] = result["environment"]
            elif 'build_workspaces' in previous:
                result['build_workspaces'] = previous['build_workspaces']
        else:
            result["build_environment"] = result["environment"]
            result["probe_sha256"] = hashlib.sha256((cli.ROOT / "benchmarks/obrador_reads.rs").read_bytes()).hexdigest()
            sources = work / "sources"
            obrador = sources / "obrador"
            casita = sources / "casita"
            result["sources"] = {"obrador": snapshot(args.obrador_source, obrador),
                                 "casita": snapshot(args.casita_source, casita)}
            save(work / "source-files.json", result["sources"])
            # Keep manifests and implementation unchanged in the original checkout.
            # Only source selection and a new benchmark example change in each copy.
            binaries = {}
            for variant in ("durable", "process"):
                variant_root = work / variant
                variant_obrador, variant_casita = variant_root / "obrador", variant_root / "casita"
                shutil.copytree(obrador, variant_obrador, symlinks=True)
                shutil.copytree(casita, variant_casita, symlinks=True)
                if variant == "durable":
                    use_durable_admission(variant_casita)
                manifest = variant_obrador / "Cargo.toml"
                package = "../casita/crates/casita" if (variant_casita / "crates/casita/Cargo.toml").exists() else "../casita"
                code, count = re.subn(r'^casita = \{[^\n]*\}$',
                    'casita = { path = "' + package + '", default-features = false, features = ["native", "experimental"] }',
                    manifest.read_text(), flags=re.MULTILINE)
                if count != 1:
                    raise common.BenchmarkError("cannot locate Obrador's native Casita dependency")
                manifest.write_text(code)
                example = variant_obrador / "obrador-core/examples/casita-retained-reads.rs"
                example.parent.mkdir(exist_ok=True)
                shutil.copy2(cli.ROOT / "benchmarks/obrador_reads.rs", example)
                command = ["cargo", "build", "--release", "--manifest-path", str(manifest),
                           "-p", "obrador-core", "--example", "casita-retained-reads",
                           "--target-dir", str(work / "target")]
                with (work / f"build-{variant}.log").open("w") as log:
                    print(f"building Obrador {variant} control", flush=True)
                    subprocess.run(command, cwd=variant_obrador, stdout=log, stderr=subprocess.STDOUT, check=True)
                binary = work / f"obrador-reads-{variant}"
                shutil.copy2(work / "target/release/examples/casita-retained-reads", binary)
                binaries[variant] = {"path": str(binary), "sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}
        result["binaries"] = binaries
        for repetition in range(args.repetitions):
            for index, (count, workers, gc) in enumerate(itertools.product(paths, args.workers, (False, True))):
                variants = ("durable", "process") if (repetition + index) % 2 == 0 else ("process", "durable")
                for variant in variants:
                    command = [binaries[variant]["path"], str(count), str(workers), str(iterations), str(gc).lower()]
                    print(f"Obrador {variant}: paths={count}, workers={workers}, gc={gc}", flush=True)
                    for attempt in range(3):
                        log = work / f"{repetition}-{count}-{workers}-{gc}-{variant}-{attempt}.log"
                        monitor = contextlib.nullcontext()
                        if args.require_quiet_host:
                            from benchmarks.host_activity import QuietHost
                            monitor = QuietHost(timeout=args.quiet_timeout)
                        failure = None
                        invocation, environment = command, None
                        if args.perf:
                            from benchmarks import obrador_profile
                            invocation, environment = obrador_profile.command(args.perf, command, log)
                        with monitor as host, log.open("w") as stream:
                            try:
                                subprocess.run(invocation, env=environment, cwd=work, stdout=stream, stderr=subprocess.STDOUT,
                                               check=True, timeout=args.timeout)
                            except (subprocess.TimeoutExpired, subprocess.CalledProcessError) as error:
                                failure = error
                        host_report = host.report() if host is not None else None
                        if host_report is not None:
                            save(log.with_suffix('.host.json'), host_report)
                        if failure is not None:
                            result.setdefault("failed_attempts", []).append({
                                "variant": variant, "paths": count, "workers": workers,
                                "concurrent_gc": gc, "repetition": repetition,
                                "error": str(failure), "log": str(log), "host_activity": host_report})
                            save(output, result)
                            if (isinstance(failure, subprocess.TimeoutExpired)
                                    and host_report is not None and not host_report['quiet']):
                                print("timeout during competing host activity; retaining failure and retrying", flush=True)
                                continue
                            raise failure
                        row = parse_sample(log.read_text(), count, workers, iterations, gc)
                        if args.perf:
                            row['cpu_profile'] = obrador_profile.report(args.perf, log, row)
                        if host is not None:
                            row["host_activity"] = host_report
                        if host is None or row["host_activity"]["quiet"]:
                            break
                        result.setdefault("contaminated_samples", []).append({**row,
                            "variant": variant, "repetition": repetition, "log": str(log)})
                        save(output, result)
                        print("competing host activity detected; retaining sample and retrying", flush=True)
                    else:
                        raise common.BenchmarkError("host activity contaminated all three attempts")
                    result["samples"].append({**row, "status": "ok", "variant": variant,
                        "operation": "obrador-read-file", "repetition": repetition, "log": str(log)})
                    save(output, result)
        for entry in binaries.values():
            if hashlib.sha256(pathlib.Path(entry["path"]).read_bytes()).hexdigest() != entry["sha256"]:
                raise common.BenchmarkError("benchmark binary changed")
        result["complete"] = True
        save(output, result)
        return 0
    except Exception as error:
        result["error"] = str(error)
        save(output, result)
        raise


if __name__ == "__main__":
    raise SystemExit(main())
