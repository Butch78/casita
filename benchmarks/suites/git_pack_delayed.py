"""Controlled local storage delays and output backpressure for Git packs."""
from __future__ import annotations

import argparse
import hashlib
import itertools
import json
import os
import pathlib
import statistics
import subprocess

from benchmarks.suites import repository as common

TEST = "git::fetch::pipeline_benchmark::controlled_reads_and_backpressure"
MODES = ("serial", "batch-8", "pipeline-8")


def check_samples(samples, repetitions):
    expected = set(itertools.product(("small", "boundary"), (1, 2, 8), (0, 5), (0, 2),
                                     MODES, range(repetitions)))
    actual = [(s["corpus"], s["permits"], s["read_ms"], s["output_ms"], s["mode"], s["repetition"])
              for s in samples if s["phase"] == "measured"]
    assert len(actual) == len(expected) and set(actual) == expected, "missing or duplicate measured cases"
    warmups = [(s["corpus"], s["permits"], s["read_ms"], s["output_ms"], s["mode"])
               for s in samples if s["phase"] == "warmup"]
    expected_warmups = {key[:-1] for key in expected}
    assert len(warmups) == len(expected_warmups) and set(warmups) == expected_warmups
    for s in samples:
        assert s["phase"] in ("warmup", "measured")
        assert s["correctness"] == "passed" and s["seconds"] > 0
        assert s["payload_reads"] == s["objects"] == (24 if s["corpus"] == "small" else 19)
        assert 0 < s["peak_readers"] <= min(s["permits"], 1 if s["mode"] == "serial" else 8)
    for corpus in ("small", "boundary"):
        assert len({s["pack_blake3"] for s in samples if s["corpus"] == corpus}) == 1
        assert len({s["output_writes"] for s in samples if s["corpus"] == corpus}) == 1, "sink chunking changed across modes"
    return samples


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--probe-binary", type=pathlib.Path, required=True)
    parser.add_argument("--repetitions", type=int, default=6)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args(argv)
    if args.repetitions < 1:
        parser.error("repetitions must be positive")
    binary = args.probe_binary.resolve()
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    log = output.with_suffix(".probe.log")
    sources = ["crates/casita/src/git/fetch/mod.rs", "crates/casita/src/git/fetch/pipeline_benchmark.rs", "crates/casita/src/git/fetch/streaming.rs",
               "Cargo.toml", "crates/casita/Cargo.toml", "Cargo.lock", "benchmarks/suites/git_pack_delayed.py"]
    root = pathlib.Path(__file__).resolve().parents[2]
    fingerprint = lambda path: hashlib.sha256(path.read_bytes()).hexdigest()
    report = {"schema_version": 1, "suite_id": "native-git", "benchmark": "git-pack-delayed",
              "status": "running", "configuration": {"network": False, "cpu_admission": False,
              "repetitions": args.repetitions, "profile": args.profile, "workers": 4,
              "permit_lifetime": "acquisition through reader drop", "read_delay_ms": [0, 5],
              "output_delay_ms_per_write": [0, 2], "max_write_bytes": 65536, "permits": [1, 2, 8],
              "modes": MODES, "order": "three rotations followed by their reverses"},
              "binary": {"path": str(binary), "sha256": fingerprint(binary)},
              "sources": {p: fingerprint(root / p) for p in sources}, "samples": []}
    def save():
        common.write_atomic(output, json.dumps(report, indent=2) + "\n")
    if output.exists() or log.exists():
        parser.error("use fresh output and log paths")
    save()
    try:
        environment = dict(os.environ, CASITA_PIPELINE_REPETITIONS=str(args.repetitions), CASITA_PIPELINE_ISOLATION="0")
        with log.open("x") as handle:
            subprocess.run([str(binary), TEST, "--exact", "--ignored", "--nocapture", "--test-threads=1"],
                           stdout=handle, stderr=subprocess.STDOUT, env=environment, check=True,
                           timeout=600 + args.repetitions * 180)
        samples = [json.loads(line.split("PIPELINE_SAMPLE ", 1)[1]) for line in log.read_text().splitlines()
                   if "PIPELINE_SAMPLE " in line]
        report["samples"] = check_samples(samples, args.repetitions)
        assert fingerprint(binary) == report["binary"]["sha256"]
        assert all(fingerprint(root / p) == h for p, h in report["sources"].items())
        comparisons = []
        for corpus, permits, read_ms, output_ms in itertools.product(("small", "boundary"), (1, 2, 8), (0, 5), (0, 2)):
            rows = [s for s in samples if s["phase"] == "measured" and
                    (s["corpus"], s["permits"], s["read_ms"], s["output_ms"]) == (corpus, permits, read_ms, output_ms)]
            medians = {m: statistics.median(s["seconds"] for s in rows if s["mode"] == m) for m in MODES}
            paired = {}
            for ref in ("serial", "batch-8"):
                ratios = []
                for repetition in range(args.repetitions):
                    indexed = {s["mode"]: s["seconds"] for s in rows if s["repetition"] == repetition}
                    ratios.append(indexed[ref] / indexed["pipeline-8"])
                paired[ref] = {"median_speedup": statistics.median(ratios), "rounds_faster": sum(x > 1 for x in ratios)}
            comparisons.append({"corpus": corpus, "permits": permits, "read_ms": read_ms, "output_ms": output_ms,
                                "median_seconds": medians, "pipeline_paired": paired})
        report["comparisons"] = comparisons
        report["status"] = "passed"
        save()
        print(json.dumps(comparisons, indent=2))
        return 0
    except (Exception, KeyboardInterrupt) as error:
        report["status"] = "failed"
        report["failure"] = str(error) or type(error).__name__
        save()
        raise


if __name__ == "__main__":
    raise SystemExit(main())
