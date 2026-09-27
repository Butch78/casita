"""Zero-delay pack controls around the streaming threshold and compression cost."""
import argparse
import hashlib
import itertools
import json
import os
import pathlib
import statistics
import subprocess

from benchmarks.suites.git_pack_delayed import TEST, MODES
from benchmarks.suites import repository as common

CORPORA = {"small": 24, "boundary": 19, "buffered": 18, "below": 1, "at": 1, "above": 1}


def validate(samples, repetitions):
    expected = set(itertools.product(CORPORA, (0, 6), MODES, range(repetitions)))
    measured = [s for s in samples if s["phase"] == "measured"]
    keys = [(s["corpus"], s["compression_level"], s["mode"], s["repetition"]) for s in measured]
    assert len(keys) == len(expected) and set(keys) == expected
    warm = [(s["corpus"], s["compression_level"], s["mode"]) for s in samples if s["phase"] == "warmup"]
    assert len(warm) == 36 and set(warm) == {k[:-1] for k in expected}
    for s in samples:
        assert s["correctness"] == "passed" and s["seconds"] > 0
        assert s["phase"] in ("warmup", "measured")
        assert s["read_ms"] == s["output_ms"] == 0
        assert s["peak_readers"] == s["permits"] == 1
        assert s["objects"] == s["payload_reads"] == CORPORA[s["corpus"]]
    comparisons = []
    for corpus, level in itertools.product(CORPORA, (0, 6)):
        rows = [s for s in samples if (s["corpus"], s["compression_level"]) == (corpus, level)]
        assert len({s["pack_blake3"] for s in rows}) == 1
        assert len({s["output_writes"] for s in rows}) == 1
        runs = [s for s in rows if s["phase"] == "measured"]
        medians = {m: statistics.median(s["seconds"] for s in runs if s["mode"] == m) for m in MODES}
        ratios = []
        for i in range(repetitions):
            round_rows = [s for s in runs if s["repetition"] == i]
            order = list(MODES[i % 3:] + MODES[:i % 3])
            if i // 3 % 2: order.reverse()
            assert [s["mode"] for s in round_rows] == order
            values = {s["mode"]: s["seconds"] for s in round_rows}
            ratios.append(values["batch-8"] / values["pipeline-8"])
        comparisons.append({"corpus": corpus, "compression_level": level, "median_seconds": medians,
                            "pipeline_reduction_percent": 100 * (1 - medians["pipeline-8"] / medians["batch-8"]),
                            "median_paired_speedup": statistics.median(ratios),
                            "pipeline_wins": sum(x > 1 for x in ratios)})
    return comparisons


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--probe-binary", type=pathlib.Path, required=True)
    parser.add_argument("--repetitions", type=int, default=30)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args(argv)
    if args.repetitions < 1: parser.error("repetitions must be positive")
    output = args.output.resolve()
    log = output.with_suffix(".probe.log")
    if output.exists() or log.exists(): parser.error("use fresh report and log paths")
    output.parent.mkdir(parents=True, exist_ok=True)
    binary = args.probe_binary.resolve()
    root = pathlib.Path(__file__).resolve().parents[2]
    sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
    sources = ["Cargo.toml", "crates/casita/Cargo.toml", "Cargo.lock", "crates/casita/src/git/fetch/mod.rs", "crates/casita/src/git/fetch/streaming.rs",
               "crates/casita/src/git/fetch/pipeline_benchmark.rs", "benchmarks/suites/git_pack_boundary.py"]
    report = {"status": "running", "schema_version": 1, "suite_id": "native-git", "benchmark": "git-pack-boundary",
              "configuration": {"network": False, "cpu_admission": False, "workers": 4,
              "repetitions": args.repetitions, "profile": args.profile, "read_ms": 0, "output_ms": 0,
              "permits": 1, "compression_levels": [0, 6], "corpora": CORPORA, "modes": MODES},
              "binary": {"path": str(binary), "sha256": sha(binary)},
              "sources": {p: sha(root/p) for p in sources}, "samples": []}
    def save(): common.write_atomic(output, json.dumps(report, indent=2) + "\n")
    save()
    try:
        environment = dict(os.environ, CASITA_PIPELINE_ISOLATION="1", CASITA_PIPELINE_REPETITIONS=str(args.repetitions))
        with log.open("x") as handle:
            subprocess.run([str(binary), TEST, "--exact", "--ignored", "--nocapture", "--test-threads=1"],
                           stdout=handle, stderr=subprocess.STDOUT, env=environment, check=True,
                           timeout=600 + args.repetitions * 30)
        report["samples"] = [json.loads(s.split("PIPELINE_SAMPLE ", 1)[1]) for s in log.read_text().splitlines() if "PIPELINE_SAMPLE " in s]
        report["comparisons"] = validate(report["samples"], args.repetitions)
        assert sha(binary) == report["binary"]["sha256"]
        assert all(sha(root/p) == h for p, h in report["sources"].items())
        report["status"] = "passed"
        save()
        print(json.dumps(report["comparisons"], indent=2))
        return 0
    except (Exception, KeyboardInterrupt) as error:
        report["status"] = "failed"
        report["failure"] = str(error) or type(error).__name__
        save()
        raise


if __name__ == "__main__":
    raise SystemExit(main())
