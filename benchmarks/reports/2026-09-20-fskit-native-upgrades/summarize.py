"""Regenerate paired latency tables from the retained five-repetition results."""
import argparse
import gzip
import sys
import json
from pathlib import Path
from statistics import median

ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT.parents[2]))


def load(name):
    with gzip.open(ROOT / name) as handle:
        result = json.load(handle)
    assert result["complete"] and result["comparison_complete"]
    assert not result.get("cleanup_errors")
    return result


def span(values, divisor):
    values = [value / divisor for value in values]
    return f"{median(values):.3f} ({min(values):.3f}–{max(values):.3f})"


def reads(result, label):
    print(f"## {label}\n")
    print("Median across five per-round p50 values, with the round range. Units: milliseconds.\n")
    print("| Case | Size / workers | Host | FSKit | FSKit / host |")
    print("| --- | --- | ---: | ---: | ---: |")
    selected = [("readdir", None, None), ("stat-256", None, None),
                ("open-read-close", 4096, None), ("open-read-close", 1048576, None),
                ("held-fd-read", 4096, None), ("held-fd-read", 1048576, None),
                ("parallel-read-32", None, 16), ("execute-script", None, None),
                ("execute-native", None, None)]
    for case, size, workers in selected:
        rows = [row for row in result["samples"]
                if (row["case"], row.get("size"), row.get("workers")) == (case, size, workers)]
        host = [row["p50_ns"] for row in rows if row["implementation"] == "host"]
        native = [row["p50_ns"] for row in rows if row["implementation"].startswith("native-")]
        if not host:
            continue
        assert len(host) == len(native) == 5
        print(f"| {case} | {size or workers or ''} | {span(host, 1e6)} | {span(native, 1e6)} | {median(native)/median(host):.2f}x |")
    print()


def workloads(result, repetitions=5):
    from benchmarks.suites.native_fskit_workloads import validate
    rows = [row for row in result["workload_trials"] if row["repetition"] < repetitions]
    validate(rows, repetitions, ("native-casita-repository", "host"))
    print("## Executable workloads\n")
    print(f"Median batch wall time in milliseconds, with the {repetitions}-round range. Fresh mount or host inode for first; immediate repeat for warm.\n")
    print("| Pattern | Workers | Phase | Host | FSKit | FSKit / host |")
    print("| --- | ---: | --- | ---: | ---: | ---: |")
    for pattern in ("shared", "distinct"):
        for workers in (1, 8, 15, 16, 17, 31, 32, 33):
            for phase in ("first", "repeat"):
                selected = [row for row in rows if row["pattern"] == pattern and row["workers"] == workers]
                host = [row[phase]["wall_ns"] for row in selected if row["implementation"] == "host"]
                native = [row[phase]["wall_ns"] for row in selected if row["implementation"] != "host"]
                assert len(host) == len(native) == repetitions
                print(f"| {pattern} | {workers} | {phase} | {span(host, 1e6)} | {span(native, 1e6)} | {median(native)/median(host):.2f}x |")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--partial", action="store_true", help="validate and show completed submatrices from the failed shared-host run")
    args = parser.parse_args()
    if args.partial:
        from benchmarks.suites import native_fskit as memory
        with gzip.open(ROOT / "shared-host-timeout.json.gz") as handle:
            repository = json.load(handle)
        assert not repository["complete"] and not repository.get("cleanup_errors")
        assert repository["native_teardown"]["repository_release_barrier"] == "passed"
        paired = [{**row, "implementation": memory.NATIVE if row["implementation"] == "native-casita-repository" else memory.HOST}
                  for row in repository["samples"] if row["case"] != "fresh-mount-first-touch" and not row["case"].endswith("-first")]
        comparisons = memory.paired_comparisons(paired, 5, repository["snapshot"]["digest"],
                                               [(case, None, None) for case in ("execute-script", "execute-native")])
        assert comparisons == repository["comparisons"] and len(comparisons) == 205
        print("# Completed submatrices from an incomplete run\n")
        print("The overall five-round workload run failed on a host awk timeout during round four. These tables validate all five ordinary-operation rounds and only the first three complete workload rounds. The incomplete fourth workload round is excluded in full. This is shared-host diagnostic evidence, not a completed full benchmark run.\n")
        reads(repository, "Repository versus host: five completed rounds")
        workloads(repository, 3)
    else:
        repository = load("repository.json.gz")
        reads(repository, "Repository versus host")
        reads(load("memory.json.gz"), "Memory fixture versus host")
        workloads(repository)
