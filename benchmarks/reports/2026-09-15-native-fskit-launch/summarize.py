"""Compare the same-binary cache controls without pooling dissimilar cases."""
import json
from pathlib import Path
from statistics import median
import sys

base = Path(__file__).parent
off = json.loads(Path(sys.argv[1] if len(sys.argv) > 1 else base / "cache-off.json").read_text())
on = json.loads(Path(sys.argv[2] if len(sys.argv) > 2 else base / "cache-on.json").read_text())
for report in (off, on):
    assert report["complete"] and report["comparison_complete"] and not report["cleanup_errors"]
for key in ("snapshot", "source_sha256", "server_sha256", "binaries", "build_identity"):
    assert off[key] == on[key], f"unmatched {key}"
assert off["configuration"]["native_directory_cache"] == "disabled"
assert on["configuration"]["native_directory_cache"] == "enabled"

def rows(report, case, implementation):
    result = [r for r in report["samples"] if r["case"] == case and r["implementation"] == implementation]
    assert len(result) == report["configuration"]["repetitions"]
    return result

print("| Case | Native cache off, ms | Native cache on, ms | fuser, ms | Host, ms |")
print("| --- | ---: | ---: | ---: | ---: |")
cases = sorted({r["case"] for r in on["samples"] if "phases" in r})
for case in cases:
    values = [median(r["p50_ns"] for r in rows(d, case, n))/1e6
              for d,n in ((off,"native-casita-repository"), (on,"native-casita-repository"),
                          (on,"fuser-casita-repository"), (on,"host"))]
    print(f"| {case} | " + " | ".join(f"{v:.3f}" for v in values) + " |")
print("\nPer-launch directory fetch counts (native):")
for case in cases:
    values = []
    for report in (off,on):
        phases = [p for r in rows(report,case,"native-casita-repository") for p in r["phases"]]
        values.append(sorted({p["counters_after"]["directories"]-p["counters_before"]["directories"] for p in phases}))
    print(case, "off", values[0], "on", values[1])
