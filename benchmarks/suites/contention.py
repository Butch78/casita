"""Independent CLI writers and snapshot readers sharing a durable repository."""
from __future__ import annotations
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import pathlib
import tempfile
import threading
from benchmarks.suites import repository as common
from benchmarks.suites.lifecycle import root_keys


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--casita-bin", type=pathlib.Path, default=pathlib.Path("target/release/casita"))
    parser.add_argument("--writers", type=int, default=4)
    parser.add_argument("--readers", type=int, default=4)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args(argv)
    if min(args.writers, args.readers, args.repetitions) < 1:
        parser.error("writer, reader, and repetition counts must be positive")
    adapter = common.CasitaAdapter(str(args.casita_bin.resolve()))
    samples = []
    with tempfile.TemporaryDirectory(prefix="casita-contention-") as temporary:
        work = pathlib.Path(temporary)
        environment = common.environment_metadata(work)
        inputs = []
        for index in range(args.writers):
            corpus = common.generate_corpus(work, f"writer-{index}", common.SCALES["smoke"]["small-files"])
            oracle = work / f"oracle-{index}"
            adapter.init(oracle)
            adapter.import_tree(oracle, corpus.base)
            inputs.append((corpus, adapter.root_key(oracle)))
        for repetition in range(1, args.repetitions + 1):
            repository = work / f"repository-{repetition}"
            adapter.init(repository)
            barrier = threading.Barrier(args.writers + args.readers)
            def sample(index):
                writer = index < args.writers
                snapshot_reader = not writer and (index - args.writers) % 2 == 0
                operation = "independent-writer" if writer else "snapshot-reader" if snapshot_reader else "integrity-reader"
                if writer:
                    command = adapter.command(repository, "import", str(inputs[index][0].base), "--root", f"bench/writer-{index}")
                elif snapshot_reader:
                    command = adapter.command(repository, "root", "ls")
                else:
                    command = adapter.command(repository, "fsck", "--audit-only")
                barrier.wait(timeout=30)
                timing = common.measured_command(common.CommandSpec([command], work, adapter.env()),
                    work / f"{repetition}-{index}.stdout", work / f"{repetition}-{index}.stderr")
                return {"status": "ok", "implementation": "casita", "operation": operation,
                    "repetition": repetition, "worker": index, "writers": args.writers, "readers": args.readers, **timing}
            with ThreadPoolExecutor(max_workers=args.writers + args.readers) as pool:
                measured = list(pool.map(sample, range(args.writers + args.readers)))
            roots = root_keys(adapter, repository)
            for index, (corpus, key) in enumerate(inputs):
                if roots.get(f"bench/writer-{index}") != key:
                    raise common.BenchmarkError("concurrent publication lost a writer's root")
                restore = work / f"restore-{repetition}-{index}"
                common.run_checked(adapter.command(repository, "checkout", key, str(restore), "--no-root"), env=adapter.env())
                common.assert_manifest(restore, corpus.base_manifest)
            common.run_checked(adapter.fsck_command(repository), env=adapter.env())
            samples.extend(measured)
    common.write_atomic(args.output, json.dumps({"schema_version": 1,
        "result_schema": "casita.process-contention.v1", "suite_id": "state-and-publication",
        "environment": environment,
        "configuration": {**vars(args), "casita_bin": str(args.casita_bin), "output": str(args.output)},
        "samples": samples}, indent=2) + "\n")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
