"""Casitar payload-size, object-count, inspection, and destination-reuse scaling."""
from __future__ import annotations

import argparse
import collections
import hashlib
import json
import math
import os
import pathlib
import random
import tempfile

from benchmarks.suites import repository as common
from benchmarks.suites.lifecycle import root_keys
from benchmarks.host_activity import QuietHost


PROFILES = {
    "smoke": {"payload_bytes": [65535, 65537, 8 * 1024**2], "file_counts": [254, 256, 1024]},
    "standard": {"payload_bytes": [65535, 65537, 64 * 1024**2, 512 * 1024**2],
                 "file_counts": [254, 256, 4096, 65536]},
}


class ArchiveAdapter(common.CasitaAdapter):
    import_profile = False

    def env(self):
        environment = super().env()
        # Pack diagnostics append non-JSON text to stdout after archive reports.
        environment.pop("CASITA_PACK_STATS", None)
        environment.pop("CASITA_CASITAR_IMPORT_PROFILE", None)
        if self.import_profile:
            environment["CASITA_CASITAR_IMPORT_PROFILE"] = "1"
        return environment


def positive_sizes(value):
    try:
        values = [int(item) for item in value.split(",")]
        if not values or min(values) < 1 or len(set(values)) != len(values):
            raise ValueError
        return values
    except ValueError as error:
        raise argparse.ArgumentTypeError("expected distinct positive comma-separated integers") from error


def build_parser():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--casita-bin", type=pathlib.Path, default=pathlib.Path("target/release/casita"))
    parser.add_argument("--profile", choices=PROFILES, default="smoke")
    parser.add_argument("--payload-bytes", type=positive_sizes)
    parser.add_argument("--file-counts", type=positive_sizes)
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--investigate-import", action="store_true",
                        help="profile 254/256-file imports (plus 4096 in standard); alternate case order")
    parser.add_argument("--pin-timing", action="store_true",
                        help="also aggregate existing pin-ledger tracing for timed imports")
    parser.add_argument("--baseline-bin", type=pathlib.Path,
                        help="compare against this binary, reversing variant order each repetition")
    parser.add_argument("--no-phase-timing", action="store_true",
                        help="keep the bounded import matrix but disable instrumentation for latency comparisons")
    parser.add_argument("--require-quiet-host", action="store_true",
                        help="Linux: require ten seconds below the CPU ceiling and enforce it throughout each case")
    parser.add_argument("--quiet-timeout", type=int, default=180,
                        help="maximum seconds to wait for quiet admission per case")
    parser.add_argument("--max-external-cpu-percent", type=float, default=40,
                        help="maximum sampled external CPU across logical CPUs (default: 40)")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    return parser


def guarded_case(adapter, directory, family, count, size, repetitions, samples,
                 repetition, quiet_timeout, activity, max_external_cpu_percent=40):
    """Retain admission evidence and reject an entire contaminated case."""
    entry = {"files": count, "file_bytes": size, "family": family,
             "variant": getattr(adapter, "variant", "single"),
             "repetition": repetition, "status": "pending", "intervals": []}
    activity.append(entry)
    monitor = QuietHost(timeout=quiet_timeout, max_cpu_fraction=max_external_cpu_percent / 100,
                        allow_competing_builds=True)
    original_sample = monitor.sample

    def recorded_sample():
        row = original_sample()
        entry["intervals"].append(row)
        return row

    monitor.sample = recorded_sample
    start = len(samples)
    try:
        with monitor:
            run_case(adapter, directory, family, count, size, repetitions, samples, repetition)
        entry["measurement"] = monitor.report()
        if not entry["measurement"]["quiet"]:
            raise common.BenchmarkError(
                f"external CPU samples did not satisfy the {max_external_cpu_percent:g}% ceiling")
        entry["status"] = "accepted"
    except Exception as error:
        entry.update(status="rejected", error=str(error))
        for row in samples[start:]:
            row["status"] = "rejected"
        raise


def fixture(tree, count, size):
    """Unique deterministic bytes; generation and SHA256 never buffer a whole file."""
    tree.mkdir()
    manifest = {}
    for index in range(count):
        name = f"file-{index:08d}"
        generator = random.Random(index)
        digest = hashlib.sha256()
        remaining = size
        with (tree / name).open("wb") as output:
            while remaining:
                block = generator.randbytes(min(remaining, 64 * 1024))
                output.write(block)
                digest.update(block)
                remaining -= len(block)
        manifest[name] = {"size": size, "sha256": digest.hexdigest()}
    return manifest


def validate_checkout(tree, manifest):
    if {path.name for path in tree.iterdir()} != set(manifest):
        raise common.BenchmarkError("checkout membership differs from fixture")
    for name, expected in manifest.items():
        path = tree / name
        if path.is_symlink() or not path.is_file() or path.stat().st_mode & 0o111:
            raise common.BenchmarkError(f"checkout type or executable bit differs: {name}")
        with path.open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        if path.stat().st_size != expected["size"] or digest != expected["sha256"]:
            raise common.BenchmarkError(f"checkout bytes differ: {name}")


def validate_report(report, expected, key, operation, reuse=None):
    validity = {"create": "verified", "inspect": "structural", "verify": "verified", "import": "imported"}[operation]
    if (report.get("schema") != "casita.archive.v1" or report.get("operation") != operation
            or report.get("validity") != validity or report.get("stats") != expected):
        raise common.BenchmarkError(f"{operation} report differs from verified archive")
    if operation != "import":
        if report.get("roots") != [key]:
            raise common.BenchmarkError(f"{operation} roots differ")
        return
    if report.get("mappings") != [{"index": 0, "name": "bench/received", "root": key}]:
        raise common.BenchmarkError("import root mapping differs")
    written, reused = report["payloads_written"], report["payloads_reused"]
    if written + reused != expected["payloads"]:
        raise common.BenchmarkError("import payload accounting differs")
    if report["records_inserted"] + report["records_reused"] != expected["records"]:
        raise common.BenchmarkError("import record accounting differs")
    if ((reuse == 0 and (reused != 0 or report["records_reused"] != 0))
            or (reuse == 50 and reused != (expected["payloads"] - 1) // 2)
            or (reuse == 100 and (written != 0 or report["records_inserted"] != 0))):
        raise common.BenchmarkError(f"destination does not exercise {reuse}% file reuse")


def corrupt_payload(archive):
    """Flip the first payload byte, retaining framing and all metadata."""
    with archive.open("r+b") as handle:
        if handle.read(8) != b"casitar1":
            raise common.BenchmarkError("unexpected archive version")
        header_bytes = int.from_bytes(handle.read(8), "little")
        handle.seek(16 + header_bytes)
        frame = handle.read(41)
        if len(frame) != 41 or frame[0] != 1 or int.from_bytes(frame[33:], "little") == 0:
            raise common.BenchmarkError("expected a nonempty first payload")
        offset = handle.tell()
        original = handle.read(1)
        if not original:
            raise common.BenchmarkError("truncated first payload")
        handle.seek(offset)
        handle.write(bytes([original[0] ^ 1]))
    return offset, original


def validate_profile(stderr, report):
    lines = [line.removeprefix("casitar_import_profile ") for line in stderr.splitlines()
             if line.startswith("casitar_import_profile ")]
    if len(lines) != 1:
        raise common.BenchmarkError("expected exactly one Casitar import phase profile")
    profile = json.loads(lines[0])
    phases = {row["phase"]: row for row in profile["phases"]}
    if (profile.get("schema_version") != 1 or len(phases) != len(profile["phases"])
            or any(row["calls"] < 1 or row["nanos"] < 0 for row in phases.values())):
        raise common.BenchmarkError("invalid import phase accounting")
    expected = {"payload_lookup": report["stats"]["payloads"],
                "stage_existing": report["stats"]["records"],
                "payload_write": report["payloads_written"],
                "payload_reuse_verify": report["payloads_reused"],
                "publish_batch": report["stats"]["records"] // 256,
                "publish_tail": int(report["stats"]["records"] % 256 != 0),
                "verify_closure": 1, "publish_roots": 1}
    if "protect_records" in phases:
        expected["protect_records"] = (report["stats"]["records"] + 255) // 256
    if any(phases.get(name, {}).get("calls", 0) != count for name, count in expected.items()):
        raise common.BenchmarkError("import phase counts differ from verified report")
    return profile


def pin_profile(stderr):
    phases = collections.defaultdict(lambda: {"calls": 0, "seconds": 0.0, "max_seconds": 0.0})
    for line in stderr.splitlines():
        if not line.startswith("{"):
            continue
        event = json.loads(line)
        if event.get("target") != "casita::pin_timing":
            continue
        fields = event["fields"]
        seconds = fields["elapsed_seconds"]
        if seconds < 0:
            raise common.BenchmarkError("negative pin phase duration")
        phase = phases[fields["phase"]]
        phase["calls"] += 1
        phase["seconds"] += seconds
        phase["max_seconds"] = max(phase["max_seconds"], seconds)
    if not phases or not phases["journal_append_sync"]["calls"]:
        raise common.BenchmarkError("missing pin journal timing events")
    return dict(phases)


def validate_pin_budget(profile, pins, records, variant=None):
    if variant == "baseline":
        return
    if not any(phase["phase"] == "protect_records" for phase in profile["phases"]):
        raise common.BenchmarkError("candidate lacks batched record-protection profile")
    batches = (records + 255) // 256
    limit = 24 + 4 * batches
    syncs = pins["journal_append_sync"]["calls"]
    if syncs > limit:
        raise common.BenchmarkError(f"fully reused import used {syncs} journal syncs; budget is {limit}")


def run_case(adapter, work, family, count, size, repetitions, samples, first_repetition=1):
    tree = work / "tree"
    manifest = fixture(tree, count, size)
    source = work / "source"
    adapter.init(source)
    adapter.import_tree(source, tree)
    key = adapter.root_key(source)
    archive = work / "fixture.casitar"
    export = adapter.command(source, "archive", "create", "--root", "bench/current",
                             "--output", str(archive), "--json")
    created = json.loads(common.run_checked(export, env=adapter.env()))
    expected = created["stats"]
    if expected["payloads"] != count + 1 or expected["records"] != count + 1:
        raise common.BenchmarkError("fixture must contain distinct files plus one directory")
    verified = json.loads(common.run_checked(adapter.cli_command("archive", "verify", str(archive), "--json"), env=adapter.env()))
    validate_report(verified, expected, key, "verify")
    if not expected["archive_digest"] or expected["archive_bytes"] != archive.stat().st_size:
        raise common.BenchmarkError("missing digest or incorrect archive size")

    def measure(operation, command, repetition, reuse=None, check=True):
        stdout, stderr = work / "stdout", work / "stderr"
        if getattr(adapter, "pin_timing", False) and operation == "import":
            command = [command[0], "--log-filter", "error,casita::pin_timing=debug",
                       "--log-format", "json", *command[1:]]
        metrics = common.measured_command(common.CommandSpec([command], work, adapter.env()), stdout, stderr, check=check)
        row = {"status": "ok", "implementation": "casita", "operation": operation,
               "family": family, "files": count, "file_bytes": size, "source_bytes": count * size,
               "seeded_file_percent": reuse, "repetition": repetition,
               "archive_bytes": expected["archive_bytes"], **metrics}
        if hasattr(adapter, "variant"):
            row["variant"] = adapter.variant
        if check:
            report = json.loads(stdout.read_text())
            validate_report(report, expected, key, operation, reuse)
            row["archive_report"] = report
            if adapter.import_profile and operation == "import":
                row["import_profile"] = validate_profile(stderr.read_text(), report)
                if getattr(adapter, "pin_timing", False):
                    row["pin_profile"] = pin_profile(stderr.read_text())
                    if reuse == 100:
                        validate_pin_budget(row["import_profile"], row["pin_profile"], report["stats"]["records"],
                                            getattr(adapter, "variant", None))
        elif metrics["exit_code"] == 0 or "payload identity mismatch" not in stderr.read_text():
            raise common.BenchmarkError("corrupted reused payload was not rejected by identity verification")
        return row

    for repetition in range(first_repetition, first_repetition + repetitions):
        archive.unlink()
        for operation, command in [("create", export), ("inspect", adapter.cli_command("archive", "inspect", str(archive), "--json"))]:
            row = measure(operation, command, repetition)
            row["correctness"] = "verified-oracle roots+counts+archive-digest"
            samples.append(row)
        for reuse in (0, 50, 100):
            with tempfile.TemporaryDirectory(prefix="destination-", dir=work) as temporary:
                destination_work = pathlib.Path(temporary)
                destination = destination_work / "repository"
                adapter.init(destination)
                if reuse == 100:
                    common.run_checked(adapter.command(destination, "archive", "import", str(archive), "--root", "bench/seed"), env=adapter.env())
                elif reuse == 50:
                    seed = destination_work / "seed"
                    seed.mkdir()
                    for name in list(manifest)[:count // 2]:
                        os.link(tree / name, seed / name)
                    adapter.import_tree(destination, seed)
                before = root_keys(adapter, destination)
                command = adapter.command(destination, "archive", "import", str(archive), "--root", "bench/received", "--json")
                row = measure("import", command, repetition, reuse)
                if root_keys(adapter, destination) != {**before, "bench/received": key}:
                    raise common.BenchmarkError("import changed unexpected roots")
                restore = destination_work / "restore"
                common.run_checked(adapter.command(destination, "checkout", key, str(restore), "--no-root"), env=adapter.env())
                validate_checkout(restore, manifest)
                common.run_checked(adapter.fsck_command(destination), env=adapter.env())
                row["correctness"] = "roots+fsck+streaming-SHA256-restore+archive-digest+reuse-counters"
                samples.append(row)
                if reuse == 100:
                    before = root_keys(adapter, destination)
                    offset, original = corrupt_payload(archive)
                    try:
                        row = measure("corrupt-reused-payload", adapter.command(destination, "archive", "import", str(archive), "--root", "bench/corrupt", "--json"), repetition, reuse, check=False)
                    finally:
                        with archive.open("r+b") as handle:
                            handle.seek(offset)
                            handle.write(original)
                    if root_keys(adapter, destination) != before:
                        raise common.BenchmarkError("corrupted archive changed roots")
                    common.run_checked(adapter.fsck_command(destination), env=adapter.env())
                    row["correctness"] = "payload-identity-rejection+unchanged-roots+fsck"
                    row["expected_failure"] = True
                    samples.append(row)


def main(argv=None):
    parser = build_parser()
    args = parser.parse_args(argv)
    if args.repetitions < 1:
        parser.error("repetitions must be positive")
    if args.quiet_timeout < 1:
        parser.error("quiet-timeout must be positive")
    if not math.isfinite(args.max_external_cpu_percent) or not 0 <= args.max_external_cpu_percent <= 100:
        parser.error("max-external-cpu-percent must be finite and between 0 and 100")
    if args.pin_timing and not args.investigate_import:
        parser.error("--pin-timing requires --investigate-import")
    if args.pin_timing and args.no_phase_timing:
        parser.error("--pin-timing conflicts with --no-phase-timing")
    if args.baseline_bin and not args.investigate_import:
        parser.error("--baseline-bin requires --investigate-import")
    payload_bytes = args.payload_bytes or PROFILES[args.profile]["payload_bytes"]
    counts = args.file_counts or PROFILES[args.profile]["file_counts"]
    if args.investigate_import:
        payload_bytes = []
        counts = args.file_counts or ([254, 256] if args.profile == "smoke" else [254, 256, 4096])
    if any(count < 2 or count % 2 for count in counts):
        parser.error("file counts must be positive even numbers for exactly half-file reuse")
    adapter = ArchiveAdapter(str(args.casita_bin.resolve()))
    adapter.import_profile = args.investigate_import and not args.no_phase_timing
    adapter.pin_timing = args.pin_timing
    with args.casita_bin.open("rb") as binary:
        binary_digest = hashlib.file_digest(binary, "sha256").hexdigest()
    adapters = [adapter]
    baseline = None
    if args.baseline_bin:
        baseline_adapter = ArchiveAdapter(str(args.baseline_bin.resolve()))
        baseline_adapter.import_profile = adapter.import_profile
        baseline_adapter.pin_timing = adapter.pin_timing
        baseline_adapter.variant = "baseline"
        adapter.variant = "candidate"
        with args.baseline_bin.open("rb") as binary:
            baseline = {"path": baseline_adapter.executable,
                        "sha256": hashlib.file_digest(binary, "sha256").hexdigest()}
        adapters = [baseline_adapter, adapter]
    samples = []
    result = {
        "schema_version": 1, "result_schema": "casita.casitar-scaling.v1", "suite_id": "casitar",
        "complete": False,
        "configuration": {"profile": args.profile, "payload_bytes": payload_bytes, "file_counts": counts,
                          "repetitions": args.repetitions, "casita_bin": adapter.executable,
                          "import_profile": adapter.import_profile,
                          "pin_timing": args.pin_timing,
                          "require_quiet_host": args.require_quiet_host,
                          "quiet_timeout": args.quiet_timeout,
                          "max_external_cpu_percent": args.max_external_cpu_percent,
                          "allow_competing_builds": True,
                          "baseline": baseline,
                          "variant_order": "alternating" if baseline else "single",
                          "case_order": "alternating" if args.investigate_import else "grouped",
                          "binary_sha256": binary_digest},
        "samples": samples,
    }
    if args.require_quiet_host:
        result["host_activity"] = []
        result["host_policy"] = ("10 seconds below the CPU ceiling before each case; "
                                 "competing processes recorded without a separate veto; "
                                 f"<={args.max_external_cpu_percent:g}% external CPU across "
                                 "logical CPUs throughout setup, measurements and verification; "
                                 "not physical host or disk isolation")

    def checkpoint():
        common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")

    try:
        with tempfile.TemporaryDirectory(prefix="casita-archive-scaling-") as temporary:
            work = pathlib.Path(temporary)
            result["environment"] = common.environment_metadata(work)
            cases = [("payload-size", 2, size) for size in payload_bytes]
            cases += [("object-count", count, 128) for count in counts]
            rounds = range(1, args.repetitions + 1) if args.investigate_import else [1]
            for repetition in rounds:
                ordered = list(reversed(cases)) if args.investigate_import and repetition % 2 == 0 else cases
                for family, count, size in ordered:
                    variants = list(reversed(adapters)) if repetition % 2 == 0 else adapters
                    for selected in variants:
                        print(f"casitar-scaling: round {repetition}, {getattr(selected, 'variant', 'single')}, {family}, {count} files x {size} bytes", flush=True)
                        with tempfile.TemporaryDirectory(prefix="case-", dir=work) as case:
                            arguments = (selected, pathlib.Path(case), family, count, size,
                                         1 if args.investigate_import else args.repetitions, samples, repetition)
                            if args.require_quiet_host:
                                guarded_case(*arguments, args.quiet_timeout, result["host_activity"],
                                             args.max_external_cpu_percent)
                            else:
                                run_case(*arguments)
                        checkpoint()
        result["complete"] = True
    finally:
        checkpoint()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
