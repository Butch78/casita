"""Alternate immutable Criterion binaries and retain correctness-gated NAR samples.

Run from the repository root with --baseline-binary, --candidate-binary,
--repetitions 4 and --output NEW_DIRECTORY. Build both revisions with the same
crates/casita/benches/nar_associations.rs before invoking this runner.
"""
from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import os
import pathlib
import platform
import statistics
import subprocess


EXPECTED = {
    f"nar_generation/local-{operation}/{files}"
    for operation in ("cached", "raw-dedup", "second-handle-cached", "scrub")
    for files in (1, 256)
} | {f"nar_generation/memory-{operation}/1" for operation in ("cached", "raw-dedup", "scrub")}
PARTIAL = {f"nar_partial/{backend}/{files}" for backend in ("local", "memory") for files in (1, 256)}


def fingerprint(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def collect(home, expected=EXPECTED):
    cases = {}
    for path in home.rglob("new/benchmark.json"):
        identifier = json.loads(path.read_text())["full_id"]
        if identifier in cases:
            raise RuntimeError(f"duplicate Criterion case: {identifier}")
        cases[identifier] = {
            "estimates_ns": json.loads(path.with_name("estimates.json").read_text()),
            "samples": json.loads(path.with_name("sample.json").read_text()),
        }
    if cases.keys() != expected:
        raise RuntimeError(f"unexpected cases: missing={expected - cases.keys()}, extra={cases.keys() - expected}")
    return cases


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline-binary", type=pathlib.Path, required=True)
    parser.add_argument("--candidate-binary", type=pathlib.Path, required=True)
    parser.add_argument("--repetitions", type=int, default=4)
    parser.add_argument("--case-set", choices=("current", "original", "partial"), default="current",
                        help="original selects archived eight cases; partial selects four enrichment cases")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    expected = EXPECTED if args.case_set == "current" else {case for case in EXPECTED if "-scrub/" not in case}
    group = "nar_generation"
    if args.case_set == "partial":
        expected, group = PARTIAL, "nar_partial"
    if args.repetitions < 2:
        parser.error("at least two repetitions are required to alternate order")
    output = args.output.resolve()
    binaries = {name: path.resolve() for name, path in (
        ("baseline", args.baseline_binary), ("candidate", args.candidate_binary))}
    identities = {name: {"path": str(path), "sha256": fingerprint(path)} for name, path in binaries.items()}
    if identities["baseline"]["sha256"] == identities["candidate"]["sha256"]:
        raise ValueError("baseline and candidate binaries are identical; rebuild in separate build directories")
    output.mkdir(parents=True, exist_ok=False)
    result = {
        "schema_version": 1,
        "started_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "host": platform.platform(),
        "cpu_count": os.cpu_count(),
        "cpu_affinity": sorted(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else None,
        "cpu_model": next((line.split(":", 1)[1].strip() for line in pathlib.Path("/proc/cpuinfo").read_text().splitlines()
                           if line.startswith("model name")), None) if pathlib.Path("/proc/cpuinfo").exists() else platform.processor(),
        "binaries": identities,
        "runs": [],
        "status": "running",
        "case_set": args.case_set,
    }

    def save():
        (output / "results.json").write_text(json.dumps(result, indent=2) + "\n")

    save()
    try:
        for name, binary in binaries.items():
            with (output / f"{name}-correctness.log").open("w") as log:
                subprocess.run([str(binary), group, "--test"],
                               stdout=log, stderr=subprocess.STDOUT, check=True, timeout=300)
        for repetition in range(args.repetitions):
            order = ("baseline", "candidate") if repetition % 2 == 0 else ("candidate", "baseline")
            for name in order:
                run_id = f"{repetition + 1}-{name}"
                print(f"Running {run_id}", flush=True)
                home = output / run_id
                command = [str(binaries[name]), "--bench", group, "--noplot"]
                load_before = os.getloadavg() if hasattr(os, "getloadavg") else None
                with (output / f"{run_id}.log").open("w") as log:
                    subprocess.run(command, env={**os.environ, "CRITERION_HOME": str(home)},
                                   stdout=log, stderr=subprocess.STDOUT, check=True, timeout=600)
                result["runs"].append({"variant": name, "repetition": repetition + 1,
                    "command": command, "load_before": load_before,
                    "load_after": os.getloadavg() if hasattr(os, "getloadavg") else None,
                    "cases": collect(home, expected)})
                save()
        result["summary"] = {}
        for case in sorted(expected):
            medians = {name: [run["cases"][case]["estimates_ns"]["median"]["point_estimate"]
                             for run in result["runs"] if run["variant"] == name] for name in binaries}
            baseline, candidate = (statistics.median(medians[name]) for name in binaries)
            result["summary"][case] = {"per_process_medians_ns": medians,
                "baseline_ns": baseline, "candidate_ns": candidate,
                "delta_ns": candidate - baseline, "change_percent": (candidate / baseline - 1) * 100,
                "paired_changes_percent": [(c / b - 1) * 100 for b, c in zip(medians["baseline"], medians["candidate"])],
                "paired_delta_ns": [c - b for b, c in zip(medians["baseline"], medians["candidate"])]}
        result["status"] = "passed"
    except BaseException as error:
        result["status"] = "failed"
        result["error"] = repr(error)
        raise
    finally:
        save()


if __name__ == "__main__":
    main()
