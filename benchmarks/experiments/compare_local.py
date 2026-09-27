#!/usr/bin/env python3
"""Alternate retained binaries through the normal validated repository suite."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("candidate", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--rounds", type=int, default=3)
    args = parser.parse_args()
    if args.rounds < 1:
        parser.error("--rounds must be positive")
    args.output.mkdir(parents=True, exist_ok=True)
    binaries = {"baseline": args.baseline.resolve(), "candidate": args.candidate.resolve()}
    provenance = {
        name: {"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
        for name, path in binaries.items()
    }
    (args.output / "binaries.json").write_text(json.dumps(provenance, indent=2) + "\n")
    for number in range(args.rounds):
        order = ["baseline", "candidate"] if number % 2 == 0 else ["candidate", "baseline"]
        for name in order:
            stem = args.output / f"{number}-{name}"
            command = [
                sys.executable, "-m", "benchmarks.cli", "run", "repository",
                "--profile", "standard", "--implementations", "casita",
                "--cache-policies", "warm", "--repetitions", "1", "--no-build",
                "--casita-bin", str(binaries[name]),
                "--output", str(stem.with_suffix(".json")),
                "--report", str(stem.with_suffix(".md")),
            ]
            print(f"round {number}: {name}", flush=True)
            with stem.with_suffix(".log").open("w") as log:
                subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, check=True)


if __name__ == "__main__":
    main()
