"""File and chunk upload concurrency with durable import and checkout gates."""
from __future__ import annotations

import argparse
import hashlib
import itertools
import json
import pathlib
import random
import shutil
import tempfile

from benchmarks.suites import repository as common
from benchmarks.suites.lifecycle import root_keys
from benchmarks.suites.metadata_collection import positive_csv

CORPORA = ("tiny", "below-chunker-minimum", "above-chunker-minimum", "large", "mixed")


def selected_corpora(value):
    names = value.split(",")
    if not names or len(names) != len(set(names)) or any(name not in CORPORA for name in names):
        raise argparse.ArgumentTypeError("expected distinct corpus names: " + ",".join(CORPORA))
    return names


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--corpora", type=selected_corpora, default=list(CORPORA))
    parser.add_argument("--file-concurrency", type=positive_csv, default=[1, 16, 32])
    parser.add_argument("--chunk-concurrency", type=positive_csv, default=[1, 32, 64])
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--casita-bin", type=pathlib.Path, default=pathlib.Path("target/release/casita"))
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path)
    parser.add_argument("--measurement-note", default="")
    args = parser.parse_args(argv)
    if args.repetitions < 1:
        parser.error("repetitions must be positive")
    if not args.no_build:
        common.run_checked(["cargo", "build", "--release", "--features", "cli", "--bin", "casita"])
    binary = args.casita_bin.resolve()
    adapter = common.CasitaAdapter(str(binary))
    with binary.open("rb") as handle:
        binary_hash = hashlib.file_digest(handle, "sha256").hexdigest()
    result = dict(schema_version=1, result_schema="casita.ingest-concurrency.v1",
                  suite_id="repository-e2e", complete=False, samples=[],
                  artifacts=[dict(path=str(binary), sha256=binary_hash)],
                  configuration=dict(profile=args.profile, corpora=args.corpora, file_concurrency=args.file_concurrency,
                                     chunk_concurrency=args.chunk_concurrency, repetitions=args.repetitions,
                                     measurement_note=args.measurement_note,
                                     timing="fresh durable CLI import; source generation, init, fsck and checkout excluded; warm OS cache"))

    def save():
        common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
        if args.report:
            lines = ["# Ingest concurrency", "", f"Complete: {result['complete']}", "",
                     result["configuration"]["timing"], "", args.measurement_note, "",
                     "| Corpus | Files | Chunks/writer | Repetition | Seconds | Peak RSS bytes |",
                     "|---|---:|---:|---:|---:|---:|"]
            for row in result["samples"]:
                lines.append(f"| {row['corpus']} | {row['file_concurrency']} | {row['chunk_concurrency']} | {row['repetition']} | {row['wall_seconds']:.4f} | {row['max_rss_bytes']} |")
            common.write_atomic(args.report, "\n".join(lines) + "\n")

    with tempfile.TemporaryDirectory(prefix="casita-ingest-concurrency-") as temporary:
        work = pathlib.Path(temporary)
        result["environment"] = common.environment_metadata(work)
        corpora = {}
        # Include both sides of the existing 128 KiB chunker minimum fast path,
        # plus enough large content to fill upload windows and many tiny files.
        for name, count, size in [
            ("tiny", 64 if args.profile == "smoke" else 4096, 1024),
            ("below-chunker-minimum", 4 if args.profile == "smoke" else 64, 128 * 1024 - 1),
            ("above-chunker-minimum", 4 if args.profile == "smoke" else 64, 128 * 1024 + 1),
            ("large", 2 if args.profile == "smoke" else 16, 20 * 1024 * 1024),
            ("mixed", 64 if args.profile == "smoke" else 512, 1024),
        ]:
            if name not in args.corpora:
                continue
            source = work / name
            source.mkdir()
            rng = random.Random(1729)
            logical_bytes = 0
            for index in range(count):
                # Stable sorted names place a large file at the start of each
                # default 16-entry window, with small files queued behind it.
                file_size = 4 * 1024 * 1024 if name == "mixed" and index % 16 == 0 else size
                (source / f"{index:05}.bin").write_bytes(rng.randbytes(file_size))
                logical_bytes += file_size
            corpora[name] = (source, common.tree_manifest(source), logical_bytes)
        expected_keys = {}
        jobs = list(itertools.product(corpora, args.file_concurrency, args.chunk_concurrency, range(args.repetitions)))
        random.Random(42).shuffle(jobs)
        save()
        try:
            for index, (name, files, chunks, repetition) in enumerate(jobs):
                print(f"ingest-concurrency: {name}, files={files}, chunks={chunks}, repetition={repetition}", flush=True)
                source, manifest, logical_bytes = corpora[name]
                repository = work / f"repository-{index}"
                adapter.init(repository)
                command = adapter.command(repository, "import", str(source), "-i", "filesystem",
                                          "--root", "bench/input", "--filesystem-rehash",
                                          "--filesystem-concurrency", str(files),
                                          "--chunk-upload-concurrency", str(chunks))
                timing = common.measured_command(common.CommandSpec([command], work, adapter.env()),
                                                 work / "stdout", work / "stderr")
                key = root_keys(adapter, repository).get("bench/input")
                if key is None or key != expected_keys.setdefault(name, key):
                    raise common.BenchmarkError("concurrency changed the imported root identity")
                restore = work / f"restore-{index}"
                common.run_checked(adapter.command(repository, "checkout", key, str(restore), "--no-root"), env=adapter.env())
                common.assert_manifest(restore, manifest)
                common.run_checked(adapter.fsck_command(repository), env=adapter.env())
                result["samples"].append(dict(status="ok", implementation="casita", operation="import",
                                              variant=f"files-{files}-chunks-{chunks}",
                                              corpus=name, file_concurrency=files, chunk_concurrency=chunks,
                                              repetition=repetition, logical_bytes=logical_bytes, root=key,
                                              correctness="reopened root identity, exact checkout manifest, fsck",
                                              command=command, **timing))
                save()
                shutil.rmtree(restore)
                shutil.rmtree(repository)
            result["complete"] = True
        except Exception as error:
            result["error"] = str(error)
            raise
        finally:
            save()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
