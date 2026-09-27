#!/usr/bin/env python3
"""Regression experiment for publication across multiple discovery batches."""

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
    parser.add_argument("--files", type=int, default=32768)
    parser.add_argument("--edits", type=int, default=4096)
    args = parser.parse_args()
    if not 1 <= args.edits <= args.files:
        parser.error("require 1 <= --edits <= --files")
    binary = args.binary.resolve()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, RUST_BACKTRACE="1")

    def run(*arguments):
        result = subprocess.run([str(binary), *map(str, arguments)], env=env, capture_output=True)
        if result.returncode:
            raise RuntimeError(result.stderr.decode(errors="replace"))
        return result.stdout.decode()

    samples = []
    with tempfile.TemporaryDirectory(prefix="casita-large-edit-") as directory:
        work = Path(directory)
        files, source = work / "files", work / "source"
        files.mkdir()
        for number in range(args.files):
            folder = files / f"{number // 128:04d}"
            folder.mkdir(exist_ok=True)
            (folder / f"{number:08d}").write_bytes(number.to_bytes(8, "little"))
        run("--repository", source, "import", files, "--root", "benchmark")
        for mode in ["exhaustive", "incremental"]:
            run("sync", "--from", source, "--to", work / mode, "--root", "benchmark")
        for number in range(args.edits):
            (files / f"{number // 128:04d}" / f"{number:08d}").write_bytes(
                b"edited" + number.to_bytes(8, "little")
            )
        run("--repository", source, "import", files, "--root", "benchmark")
        expected = {path.relative_to(files): path.read_bytes() for path in files.rglob("*") if path.is_file()}
        for mode in ["exhaustive", "incremental"]:
            destination = work / mode
            command = [str(binary), "--log-filter", "casita::transfer=info", "--log-format", "json",
                       "sync", "--from", str(source), "--to", str(destination), "--root", "benchmark"]
            if mode == "incremental":
                command.append("--incremental")
            metrics = measured_command(CommandSpec([command], Path.cwd(), env),
                                       output / f"{mode}.stdout", output / f"{mode}.stderr")
            events = [json.loads(line) for line in (output / f"{mode}.stderr").read_text().splitlines()
                      if line.startswith("{")]
            fields = next(event["fields"] for event in events
                          if event.get("fields", {}).get("message") == "transfer completed")
            changed_groups = (args.edits + 127) // 128
            assert fields["published_objects"] == args.edits + changed_groups + 1
            discovered_files = args.files if mode == "exhaustive" else min(args.files, changed_groups * 128)
            assert fields["discovered_objects"] == discovered_files + (args.files + 127) // 128 + 1
            run("--repository", destination, "fsck")
            key = run("--repository", destination, "root", "ls", "benchmark").split()[0]
            restored = work / f"restored-{mode}"
            run("--repository", destination, "checkout", key, restored, "--no-root")
            actual = {path.relative_to(restored): path.read_bytes() for path in restored.rglob("*") if path.is_file()}
            assert actual == expected
            samples.append({"mode": mode, "metrics": metrics, "transfer": fields, "validated": True})
            print(json.dumps(samples[-1]), flush=True)
    result = {"files": args.files, "edited_files": args.edits, "samples": samples,
              "binary": str(binary), "sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}
    (output / "result.json").write_text(json.dumps(result, indent=2) + "\n")


if __name__ == "__main__":
    main()
