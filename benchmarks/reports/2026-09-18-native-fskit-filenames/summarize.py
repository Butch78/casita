"""Validate same-binary filename controls and summarize launch/callback latency."""
import json
import runpy
from pathlib import Path
from statistics import median

ROOT = Path(__file__).resolve().parent


def summarize(report, case, backend="native-casita-repository"):
    rows = [r for r in report["samples"] if r["case"] == case and r["implementation"] == backend]
    values = []
    for row in rows:
        for sample in row["phases"]:
            before, after = (sample[k].get("native_enumerations", {}) for k in ("counters_before", "counters_after"))
            values.append([sum(v[i] for v in after.values()) - sum(v[i] for v in before.values()) for i in (0, 2)])
    return (median(r["p50_ns"] for r in rows)/1e6,
            median(v[0] for v in values), median(v[1] for v in values)/1e6)


if __name__ == "__main__":
    identity = None
    for mode in ("data", "bytes"):
        aggregate = json.loads((ROOT / f"filename-{mode}.json").read_text())
        assert aggregate["complete"] and len(aggregate["runs"]) == 4
    for count in (0, 128, 256, 512):
        reports = []
        for mode in ("data", "bytes"):
            report = json.loads((ROOT / f"filename-{mode}-{count}.json").read_text())
            assert report["complete"] and report["comparison_complete"] and not report["cleanup_errors"]
            assert report["configuration"]["native_filename_construction"] == mode
            assert report["configuration"]["native_enumeration_timing"] == "basic"
            current = tuple(report[k] for k in ("source_sha256", "build_identity", "binaries", "server_sha256"))
            if identity is None:
                identity = current
            assert current == identity, "build differs"
            reports.append(report)
        assert reports[0]["snapshot"] == reports[1]["snapshot"], "snapshot differs"
        for case in ("script-timeout", "native-timeout", "native-static-code"):
            a, b = [summarize(r, case) for r in reports]
            fuser = summarize(reports[1], case, "fuser-casita-repository")[0]
            host = summarize(reports[1], case, "host")[0]
            assert a[1] == b[1], "enumeration count changed"
            print(count, case, f"data={a[0]:.3f}ms bytes={b[0]:.3f}ms fuser={fuser:.3f}ms host={host:.3f}ms",
                  f"callbacks={a[1]:g} callback-ms={a[2]:.3f}/{b[2]:.3f}")
    phases = runpy.run_path(str(ROOT.parent / "2026-09-18-native-fskit-phases/summarize.py"))
    for mode in ("bytes", "data"):
        report = json.loads((ROOT / f"filename-phases-{mode}.json").read_text())
        phases["validate"](report)
        assert report["configuration"]["native_filename_construction"] == mode
        assert report["configuration"]["native_enumeration_timing"] == "detailed"
        assert tuple(report[k] for k in ("source_sha256", "build_identity", "binaries", "server_sha256")) == identity
        assert report["snapshot"] == reports[0]["snapshot"]
        for case in ("script-timeout", "native-timeout", "native-static-code"):
            samples = [s for row in report["samples"] if row["case"] == case
                       and row["implementation"] == "native-casita-repository" for s in row["phases"]]
            parts = [median(phases["delta"](s, "native_enumeration_phases", 5)[i] for s in samples)/1e6 for i in range(4)]
            print("detailed", mode, case, "setup/name/pack/drop ms=" + "/".join(f"{v:.3f}" for v in parts))
