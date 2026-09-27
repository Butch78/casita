"""Validate phase accounting and summarize retained same-binary density runs."""
import json
from pathlib import Path
from statistics import median

ROOT = Path(__file__).resolve().parent


def delta(sample, key, width):
    before, after = (sample[k].get(key, {}) for k in ("counters_before", "counters_after"))
    return [sum(v[i] for v in after.values()) - sum(v[i] for v in before.values()) for i in range(width)]


def validate(report):
    assert report["complete"] and report["comparison_complete"] and not report["cleanup_errors"]
    detailed = report["configuration"]["native_enumeration_timing"] == "detailed"
    for row in report["samples"]:
        if row["implementation"] != "native-casita-repository" or "phases" not in row:
            continue
        for sample in row["phases"]:
            enumeration = delta(sample, "native_enumerations", 4)
            phases = delta(sample, "native_enumeration_phases", 5)
            assert min(phases) >= 0
            if detailed:
                assert sum(phases[:4]) <= enumeration[2], "phase time exceeds callback time"
                assert enumeration[1] <= phases[4] <= enumeration[1] + enumeration[0], "invalid pack attempt count"
            else:
                assert not any(phases), "disabled timing produced phase counters"


if __name__ == "__main__":
    identity = None
    snapshots = {}
    for mode in ("phases", "basic"):
        aggregate = json.loads((ROOT / f"enumeration-{mode}.json").read_text())
        assert aggregate["complete"] and len(aggregate["runs"]) == 4
        for count in (0, 128, 256, 512):
            report = json.loads((ROOT / f"enumeration-{mode}-{count}.json").read_text())
            validate(report)
            current = tuple(report[k] for k in ("source_sha256", "build_identity", "binaries", "server_sha256"))
            if identity is None:
                identity = current
            assert current == identity
            assert snapshots.setdefault(count, report["snapshot"]) == report["snapshot"]
            for case in ("script-timeout", "native-timeout", "native-static-code"):
                rows = [r for r in report["samples"] if r["case"] == case and r["implementation"] == "native-casita-repository"]
                samples = [s for r in rows for s in r["phases"]]
                parts = [median(delta(s, "native_enumeration_phases", 5)[i] for s in samples) for i in range(5)]
                callback = median(delta(s, "native_enumerations", 4)[2] for s in samples) / 1e6
                latency = median(r["p50_ns"] for r in rows) / 1e6
                print(mode, count, case, f"launch={latency:.3f}ms callback={callback:.3f}ms",
                      "setup/name/pack/drop ms=" + "/".join(f"{v/1e6:.3f}" for v in parts[:4]),
                      f"attempts={parts[4]:g}")
