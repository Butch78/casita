#!/usr/bin/env python3
"""Measure unchanged sync discovery and forced spilling, with fsck/restore gates.

Run as python3 -m benchmarks.experiments.transfer --binary ... --output ...
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile

from benchmarks.suites.repository import CommandSpec, measured_command


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--sizes", default="4096,32768")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--variants", default="exhaustive-memory,exhaustive-spill,incremental")
    args = parser.parse_args()
    if args.repetitions < 1:
        parser.error("--repetitions must be positive")
    sizes = [int(size) for size in args.sizes.split(",")]
    if not sizes or any(size < 1 for size in sizes):
        parser.error("--sizes must contain positive file counts")
    selected_variants = set(args.variants.split(","))
    if not selected_variants <= {"exhaustive-memory", "exhaustive-spill", "incremental"}:
        parser.error("--variants contains an unknown transfer mode")
    binary = str(args.binary.resolve())
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    samples = []

    def run(*arguments):
        return subprocess.run([binary, *map(str, arguments)], check=True, capture_output=True)

    for size in sizes:
        with tempfile.TemporaryDirectory(prefix="casita-transfer-experiment-") as directory:
            work = Path(directory)
            files, source, destination = (work / name for name in ("files", "source", "destination"))
            files.mkdir()
            for number in range(size):
                folder = files / f"{number // 128:04d}"
                folder.mkdir(exist_ok=True)
                (folder / f"{number:08d}").write_bytes(number.to_bytes(8, "little"))
            run("--repository", source, "import", files, "--root", "benchmark")
            run("sync", "--from", source, "--to", destination, "--root", "benchmark")
            variants = [
                ("exhaustive-memory", 250_000, False),
                ("exhaustive-spill", 128, False),
                ("incremental", 128, True),
            ]
            variants = [item for item in variants if item[0] in selected_variants]
            for repetition in range(args.repetitions):
                order = variants if repetition % 2 == 0 else list(reversed(variants))
                for name, threshold, incremental in order:
                    command = [binary, "--log-filter", "casita::transfer=info", "--log-format", "json",
                               "--spill-memory-objects", str(threshold), "sync", "--from", str(source),
                               "--to", str(destination), "--root", "benchmark"]
                    if incremental:
                        command.append("--incremental")
                    stem = output / f"{size}-{repetition}-{name}"
                    metrics = measured_command(
                        CommandSpec([command], Path.cwd(), dict(os.environ)),
                        stem.with_suffix(".stdout"), stem.with_suffix(".stderr"),
                    )
                    events = [json.loads(line) for line in stem.with_suffix(".stderr").read_text().splitlines()
                              if line.startswith("{")]
                    completed = [event["fields"] for event in events
                                 if event.get("fields", {}).get("message") == "transfer completed"]
                    assert len(completed) == 1, events
                    fields = completed[0]
                    assert fields["published_objects"] == 0
                    discovered = 1 if incremental else size + (size + 127) // 128 + 1
                    assert fields["discovered_objects"] == discovered
                    assert (fields.get("spill_files", 0) > 0) == (discovered >= threshold)
                    samples.append({"files": size, "variant": name, "repetition": repetition,
                                    "metrics": metrics, "transfer": fields, "command": command})
                    print(json.dumps(samples[-1]), flush=True)
            run("--repository", destination, "fsck")
            restored = work / "restored"
            roots = run("--repository", destination, "root", "ls", "benchmark").stdout.decode()
            key = next(line.split()[0] for line in roots.splitlines() if line.endswith("  benchmark"))
            run("--repository", destination, "checkout", key, restored, "--no-root")
            actual = {path.relative_to(restored): path.read_bytes() for path in restored.rglob("*") if path.is_file()}
            expected = {path.relative_to(files): path.read_bytes() for path in files.rglob("*") if path.is_file()}
            assert actual == expected
    result = {"binary": binary, "sha256": hashlib.sha256(Path(binary).read_bytes()).hexdigest(),
              "samples": samples, "validated": True, "validation": "full fsck and exact restored file contents"}
    (output / "result.json").write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()
