"""Scoped catalog replay across count- and byte-triggered carry boundaries."""
from __future__ import annotations
import argparse
import hashlib
import json
import os
import pathlib
import subprocess
from benchmarks import cli
from benchmarks.suites import repository as common
from benchmarks.suites.metadata_collection import CARGO_ARGUMENTS
from benchmarks.suites.pack.catalog import parse_probe_binary

PROBE = "blob::pack::benchmarks::benchmark_scoped_catalog"
CORRECTNESS = "exact sentinel bytes; growing chunk visibility; manifest visibility; all leases released"
COUNTS = (8, 1023, 1024, 1025)
CASES = ("manifests", "packs-below", "packs-above", "packs-count-below", "packs-count-above")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--cases", nargs="+", choices=CASES, default=list(CASES))
    parser.add_argument("--iterations", type=int)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--probe-binary", type=pathlib.Path)
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path)
    args = parser.parse_args(argv)
    iterations = args.iterations if args.iterations is not None else (8 if args.profile == "smoke" else 64)
    if min(iterations, args.repetitions) < 1:
        parser.error("iterations and repetitions must be positive")
    if args.no_build and args.probe_binary is None:
        parser.error("--no-build requires --probe-binary")
    configurations = [(case, count) for case in args.cases for count in (COUNTS if case == "manifests" else (0,))]
    binary = args.probe_binary
    if binary is None:
        built = subprocess.run(["cargo", *CARGO_ARGUMENTS], cwd=cli.ROOT, capture_output=True, text=True)
        if built.returncode:
            raise common.BenchmarkError(built.stderr or built.stdout)
        binary = parse_probe_binary(built.stdout)
    binary = binary.resolve()
    with binary.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    result = dict(schema_version=1, suite_id="state-and-publication", complete=False,
                  environment=common.environment_metadata(cli.ROOT),
                  configuration=dict(iterations=iterations, repetitions=args.repetitions, cases=args.cases, publications=COUNTS),
                  artifacts=[dict(path=str(binary), sha256=digest)], samples=[], processes=[])

    def save():
        common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
        lines = ["# Scoped catalog replay", "", f"Complete: {result['complete']}", "",
                 "Admission timing only. Every snapshot checks a packed sentinel and manifest visibility outside timing; growing cases also check new-chunk visibility.",
                 "Alternating catalogs intentionally evict the single cached view. All leases must be released.", "",
                 "| Case | Publications | Inline deltas | Runs | Mode | Rep | Seconds/open |",
                 "|---|---:|---:|---:|---|---:|---:|"]
        for sample in result["samples"]:
            lines.append(f"| {sample['case']} | {sample['publications']} | {sample['inline_deltas']} | {sample['runs']} | {sample['variant']} | {sample['repetition']} | {sample['wall_seconds']:.9f} |")
        if "error" in result:
            lines += ["", result["error"]]
        common.write_atomic(args.report or args.output.with_suffix(".md"), "\n".join(lines) + "\n")

    save()
    try:
        for repetition in range(1, args.repetitions + 1):
            for case, count in configurations:
                print(f"scoped-catalog: case={case} publications={count} rep={repetition}", flush=True)
                env = {**os.environ, "CASITA_SCOPED_CASE": case, "CASITA_SCOPED_PUBLICATIONS": str(count), "CASITA_SCOPED_ITERATIONS": str(iterations)}
                process = subprocess.run([str(binary), PROBE, "--exact", "--ignored", "--nocapture"],
                                         env=env, capture_output=True, text=True, timeout=300)
                result["processes"].append(dict(case=case, publications=count, repetition=repetition,
                    exit_code=process.returncode, stdout=process.stdout, stderr=process.stderr))
                rows = [json.loads(line.removeprefix("scoped_catalog_sample ")) for line in process.stdout.splitlines()
                        if line.startswith("scoped_catalog_sample ")]
                if process.returncode or len(rows) != 1 or "test result: ok. 1 passed; 0 failed;" not in process.stdout:
                    raise common.BenchmarkError(f"scoped catalog probe failed: {process.stderr}\n{process.stdout}")
                sample = rows[0]
                if sample.get("correctness") != CORRECTNESS or sample.get("case") != case or sample.get("iterations") != iterations:
                    raise common.BenchmarkError("scoped catalog correctness or configuration mismatch")
                if case == "manifests":
                    if sample.get("publications") != count or sample.get("inline_deltas") != (count if count <= 1024 else 0) or sample.get("runs") != (0 if count <= 1024 else 1):
                        raise common.BenchmarkError("fixture did not cross the expected inline-delta boundary")
                else:
                    carry = sample.get("carry_at")
                    below = case.endswith("below")
                    chunks_per_pack = 1 if case.startswith("packs-count-") else 64
                    expected_deltas = sample.get("publications") if below else 0
                    if (type(carry) is not int or carry <= 2 or sample.get("chunks_per_pack") != chunks_per_pack
                            or sample.get("publications") != carry - int(below)
                            or sample.get("runs") != (0 if below else 1)
                            or sample.get("inline_deltas") != expected_deltas):
                        raise common.BenchmarkError("pack fixture did not cross the expected carry boundary")
                    if case.startswith("packs-count-") and carry != 1025:
                        raise common.BenchmarkError("small-pack fixture missed the inline-delta count boundary")
                for field in ("cold_nanos", "stable_nanos", "alternating_nanos", "catalog_bytes"):
                    if type(sample.get(field)) is not int or sample[field] <= 0:
                        raise common.BenchmarkError(f"invalid {field}")
                for mode in ("cold", "stable", "alternating"):
                    result["samples"].append(dict(sample, status="ok", operation="scoped-catalog", variant=mode,
                        entries=sample["publications"], repetition=repetition,
                        wall_seconds=sample[f"{mode}_nanos"] / 1e9 / (1 if mode == "cold" else iterations)))
                save()
        result["complete"] = True
    except Exception as error:
        result["error"] = str(error)
        raise
    finally:
        save()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
