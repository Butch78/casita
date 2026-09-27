"""Summarize completed paired results; never average dissimilar workloads."""
import json
from pathlib import Path
from statistics import median
import sys

report = json.loads(Path(sys.argv[1]).read_text())
assert report["complete"] and report["comparison_complete"]
dimensions = [
    ("readdir", None, None), ("stat-256", None, None),
    ("open-read-close", 4096, None), ("open-read-close", 65536, None),
    ("open-read-close", 1048576, None), ("held-fd-read", 4096, None),
    ("held-fd-read", 1048576, None),
    *[("parallel-read-32", None, workers) for workers in (1, 4, 16)],
    ("execute-script", None, None), ("execute-native", None, None),
]
print("| Case (size / workers) | Native p50, ms | fuser p50, ms | Paired fuser/native median (range) |")
print("| --- | ---: | ---: | ---: |")
for case, size, workers in dimensions:
    def matches(row):
        return (row["case"], row.get("size"), row.get("workers")) == (case, size, workers)
    samples = [row for row in report["samples"] if matches(row)]
    native = [row["p50_ns"] / 1e6 for row in samples if row["implementation"] == "native-casita-repository"]
    fuser = [row["p50_ns"] / 1e6 for row in samples if row["implementation"] == "fuser-casita-repository"]
    ratios = [row["p50_fuser_over_native"] for row in report["comparisons"] if matches(row)]
    assert len(native) == len(fuser) == len(ratios) == report["configuration"]["repetitions"]
    label = case + (f" / {size} B" if size is not None else f" / {workers} workers" if workers is not None else "")
    print(f"| {label} | {median(native):.4f} | {median(fuser):.4f} | {median(ratios):.2f}× ({min(ratios):.2f}–{max(ratios):.2f}) |")
print("\nBlob opens per timed round:")
for row in report["rounds"]:
    print(row["repetition"], {name: row[name]["after"]["blob_opens"] - row[name]["before"]["blob_opens"]
          for name in ("native-casita-repository", "fuser-casita-repository")})
