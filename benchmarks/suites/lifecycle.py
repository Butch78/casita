"""Archive, interrupted receiver, and retained-generation lifecycle benchmarks.

Every successful sample is gated by root identity, fsck, and restored bytes.
Preparation and validation are outside the timed native command.
"""
from __future__ import annotations
import argparse
import json
import os
import pathlib
import shutil
import subprocess
import tempfile
import time
from benchmarks.suites import repository as common


def root_keys(adapter, repository):
    output = common.run_checked(adapter.command(repository, "root", "ls"), env=adapter.env())
    return {line.split()[1]: line.split()[0] for line in output.splitlines() if len(line.split()) == 2 and ":" in line.split()[0]}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=("casitar", "recovery", "generations"), default="casitar")
    parser.add_argument("--casita-bin", type=pathlib.Path, default=pathlib.Path("target/release/casita"))
    parser.add_argument("--profile", choices=("smoke", "standard"), default="smoke")
    parser.add_argument("--generations", type=int, default=10)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args(argv)
    if min(args.generations, args.repetitions) < 1:
        parser.error("generations and repetitions must be positive")
    adapter = common.CasitaAdapter(str(args.casita_bin.resolve()))
    samples = []
    with tempfile.TemporaryDirectory(prefix="casita-lifecycle-") as temporary:
        work = pathlib.Path(temporary)
        environment = common.environment_metadata(work)
        corpus = common.generate_corpus(work, "mixed", common.SCALES[args.profile]["mixed"])
        source = work / "source"
        adapter.init(source)
        adapter.import_tree(source, corpus.base)
        key = adapter.root_key(source)
        archive = work / "base.casitar"
        export = adapter.command(source, "archive", "create", "--root", "bench/current", "--output", str(archive))
        common.run_checked(export, env=adapter.env())
        common.run_checked(adapter.command(source, "archive", "verify", str(archive)), env=adapter.env())
        tar_archive = work / "base.tar"
        if args.mode == "casitar":
            common.run_checked(["tar", "--format=ustar", "-cf", str(tar_archive), "-C", str(corpus.base),
                "--", *sorted(path.name for path in corpus.base.iterdir())])

        def timed(operation, repository, command, repetition, *, expected_key=key, expected_manifest=corpus.base_manifest, check=True, extra=None):
            number = len(samples)
            stdout, stderr = work / f"{number}.stdout", work / f"{number}.stderr"
            result = common.measured_command(common.CommandSpec([command], work, adapter.env()), stdout, stderr, check=check)
            if not check and result["exit_code"] == 0:
                raise common.BenchmarkError(f"{operation} accepted invalid input")
            roots = root_keys(adapter, repository)
            if expected_key is not None:
                if expected_key not in roots.values():
                    raise common.BenchmarkError(f"{operation} did not publish the expected root")
                restore = work / f"restore-{number}"
                common.run_checked(adapter.command(repository, "checkout", expected_key, str(restore), "--no-root"), env=adapter.env())
                common.assert_manifest(restore, expected_manifest)
            elif roots:
                raise common.BenchmarkError(f"{operation} published a root from an incomplete stream")
            common.run_checked(adapter.fsck_command(repository), env=adapter.env())
            samples.append({"status": "ok", "implementation": "casita", "operation": operation,
                "repetition": repetition, "source_bytes": corpus.base_bytes,
                "archive_bytes": archive.stat().st_size, "correctness": "root+fsck+restore",
                "expected_failure": not check, **result, **(extra or {})})

        for repetition in range(1, args.repetitions + 1):
            destination = work / f"destination-{repetition}"
            adapter.init(destination)
            ingest = adapter.command(destination, "archive", "import", str(archive), "--root-prefix", "bench/received")
            if args.mode == "casitar":
                archive.unlink()
                timed("export", source, export, repetition)
                timed("import", destination, ingest, repetition)
                timed("duplicate-import", destination, ingest[:-1] + ["bench/duplicate"], repetition)
                tar_destination = work / f"tar-destination-{repetition}"
                adapter.init(tar_destination)
                timed("tar-import", tar_destination, adapter.command(tar_destination,
                    "import", str(tar_archive), "--importer", "tar", "--root", "bench/tar"), repetition,
                    extra={"archive_bytes": tar_archive.stat().st_size})
                damaged = work / "malformed.casitar"
                damaged.write_bytes(b"invalid archive\0")
                before = root_keys(adapter, destination)
                timed("malformed-stream", destination, adapter.command(destination, "archive", "import", str(damaged)), repetition, check=False)
                if root_keys(adapter, destination) != before:
                    raise common.BenchmarkError("malformed stream changed roots")
            elif args.mode == "recovery":
                # Cut before the final End frame. A bounded pipe forces the
                # receiver to consume input before it can be interrupted.
                partial = archive.read_bytes()[:-1]
                with (work / "interrupted.stdout").open("wb") as out, (work / "interrupted.stderr").open("wb") as err:
                    process = subprocess.Popen(adapter.command(destination, "archive", "import", "-", "--root-prefix", "bench/received"),
                        stdin=subprocess.PIPE, stdout=out, stderr=err, env=adapter.env())
                    try:
                        process.stdin.write(partial)
                        process.stdin.flush()
                        if process.poll() is not None:
                            raise common.BenchmarkError("receiver exited before interruption")
                        process.kill()
                        process.wait(timeout=30)
                    finally:
                        process.stdin.close()
                        if process.poll() is None:
                            process.kill()
                            process.wait()
                if root_keys(adapter, destination):
                    raise common.BenchmarkError("interrupted stream published a root")
                timed("restart-and-reimport", destination, ingest, repetition,
                    extra={"interrupted_input_bytes": len(partial), "interruption": "before EOF; staging position is unspecified"})
                damaged = work / "truncated.casitar"
                damaged.write_bytes(partial)
                empty = work / f"truncated-{repetition}"
                adapter.init(empty)
                timed("truncated-stream", empty, adapter.command(empty, "archive", "import", str(damaged)), repetition, expected_key=None, check=False)
            else:
                source_tree = work / f"growing-{repetition}"
                shutil.copytree(corpus.base, source_tree, symlinks=True)
                adapter.import_tree(destination, source_tree)
                for generation in range(args.generations):
                    (source_tree / "generation").write_text(str(generation))
                    expected_manifest = common.tree_manifest(source_tree)
                    # An oracle repository determines the canonical root outside
                    # timing, using an independent repository and ingest cache.
                    adapter.import_tree(source, source_tree)
                    expected_key = adapter.root_key(source)
                    timed("tiny-delta-import", destination,
                        adapter.command(destination, "import", str(source_tree), "--root", f"bench/generation-{generation}"), repetition,
                        expected_key=expected_key, expected_manifest=expected_manifest, extra={"generation": generation})
                    timed("unchanged-import", destination,
                        adapter.command(destination, "import", str(source_tree), "--root", f"bench/generation-{generation}"), repetition,
                        expected_key=expected_key, expected_manifest=expected_manifest, extra={"generation": generation})
    common.write_atomic(args.output, json.dumps({"schema_version": 1, "result_schema": "casita.lifecycle.v1",
        "environment": environment,
        "suite_id": {"casitar": "casitar", "recovery": "fault-and-recovery", "generations": "huge-repositories"}[args.mode],
        "configuration": {**vars(args), "casita_bin": str(args.casita_bin), "output": str(args.output)},
        "samples": samples}, indent=2) + "\n")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
