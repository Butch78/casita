"""Summarize retained launch reports; run with Python 3 from any directory."""
import json
from pathlib import Path
from statistics import median

ROOT = Path(__file__).resolve().parent
CASES = ("script-timeout", "script-blocking", "script-interpreted-blocking",
         "native-timeout", "native-blocking", "native-posix-spawn-blocking",
         "native-getxattr", "native-listxattr", "native-realpath",
         "native-getattrname", "native-getattrpath", "native-static-code")


def summarize(path):
    report = json.loads(path.read_text())
    assert report["complete"] and report["comparison_complete"], path
    assert not report["cleanup_errors"], path
    print(f"\n{path.name}")
    for case in CASES:
        rows = [row for row in report["samples"] if row["case"] == case]
        for backend in sorted({row["implementation"] for row in rows}):
            selected = [row for row in rows if row["implementation"] == backend]
            latency = median(row["p50_ns"] for row in selected) / 1e6
            calls = []
            callback_ns = []
            for row in selected:
                for sample in row["phases"]:
                    before = sample["counters_before"].get("native_enumerations", {})
                    after = sample["counters_after"].get("native_enumerations", {})
                    calls.append(sum(v[0] for v in after.values()) - sum(v[0] for v in before.values()))
                    callback_ns.append(sum(v[2] for v in after.values()) - sum(v[2] for v in before.values()))
            detail = (f"enum calls={median(calls):g}, callback={median(callback_ns)/1e6:.3f} ms"
                      if backend == "native-casita-repository" else "enumeration not instrumented")
            print(f"{case:20} {backend:25} {latency:9.3f} ms; {detail}")
    return report


if __name__ == "__main__":
    baseline = None
    snapshots = {}
    for path in sorted(ROOT.glob("xattrs-*.json")) + sorted(ROOT.glob("density-*-*.json")):
        report = summarize(path)
        identity = tuple(report[key] for key in ("binaries", "server_sha256", "source_sha256", "build_identity"))
        if baseline is None:
            baseline = identity
        assert identity == baseline, f"build differs: {path}"
        count = report["configuration"]["metadata_files"]
        snapshot = report["snapshot"]
        assert snapshots.setdefault(count, snapshot) == snapshot, f"snapshot differs: {path}"
    print("\nAll summarized runs passed gates and share source/build/binary identities.")
    # Later controls extend the harness, so their source receipts differ.
    for name in ("path-controls.json", "security-controls.json"):
        if (ROOT / name).exists():
            summarize(ROOT / name)
