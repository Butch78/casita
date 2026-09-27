"""Verify same-build controls and print median round p50 launch/callback times."""
import json
from pathlib import Path
from statistics import median

ROOT = Path(__file__).resolve().parent


def rows(report, case, backend="native-casita-repository"):
    return [r for r in report["samples"] if r["case"] == case and r["implementation"] == backend]


def latency(report, case, backend="native-casita-repository"):
    return median(r["p50_ns"] for r in rows(report, case, backend)) / 1e6


def callbacks(report, case, index):
    values = []
    for row in rows(report, case):
        for phase in row["phases"]:
            before, after = (phase[k]["native_enumerations"] for k in ("counters_before", "counters_after"))
            values.append(sum(v[index] for v in after.values()) - sum(v[index] for v in before.values()))
    return median(values)


if __name__ == "__main__":
    identity = None
    for count in (0, 128, 256, 512):
        off, on = [json.loads((ROOT / f"entry-cache-{mode}-{count}.json").read_text()) for mode in ("off-retry", "on")]
        for report, mode in ((off, "disabled"), (on, "enabled")):
            assert report["complete"] and report["comparison_complete"] and not report["cleanup_errors"]
            assert report["configuration"]["native_enumeration_cache"] == mode
            current = tuple(report[k] for k in ("source_sha256", "build_identity", "binaries", "server_sha256"))
            if identity is None:
                identity = current
            assert current == identity, "build differs"
        assert off["snapshot"] == on["snapshot"], "snapshot differs"
        for case in ("script-timeout", "native-timeout", "native-static-code"):
            print(count, case, f"off={latency(off, case):.3f}ms on={latency(on, case):.3f}ms",
                  f"fuser={latency(on, case, 'fuser-casita-repository'):.3f}ms",
                  f"callbacks={callbacks(off, case, 0):g}/{callbacks(on, case, 0):g}",
                  f"callback ms={callbacks(off, case, 2)/1e6:.3f}/{callbacks(on, case, 2)/1e6:.3f}")
