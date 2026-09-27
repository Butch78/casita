"""Verify capability control receipts and print VFS flags and launch results."""
import json
from pathlib import Path
from statistics import median

ROOT = Path(__file__).resolve().parent


if __name__ == "__main__":
    reports = [json.loads((ROOT / f"capabilities-{mode}.json").read_text()) for mode in ("minimal", "explicit")]
    for key in ("source_sha256", "build_identity", "binaries", "server_sha256", "snapshot"):
        assert reports[0][key] == reports[1][key], key
    for mode, report in zip(("minimal", "explicit"), reports):
        assert report["complete"] and report["comparison_complete"] and not report["cleanup_errors"]
        assert report["configuration"]["native_volume_capabilities"] == mode
        for name, flags in report["volume_capabilities"].items():
            print(mode, name, "format capabilities", hex(flags["capabilities"][0]), "valid", hex(flags["valid"][0]))
        for case in ("script-timeout", "native-timeout", "native-static-code"):
            for backend in ("native-casita-repository", "fuser-casita-repository", "host"):
                rows = [r for r in report["samples"] if r["case"] == case and r["implementation"] == backend]
                latency = median(r["p50_ns"] for r in rows)/1e6
                calls = []
                for row in rows:
                    for sample in row["phases"]:
                        before, after = (sample[k].get("native_enumerations", {}) for k in ("counters_before", "counters_after"))
                        calls.append(sum(v[0] for v in after.values())-sum(v[0] for v in before.values()))
                detail = f"native callbacks={median(calls):g}" if backend == "native-casita-repository" else ""
                print(mode, case, backend, f"{latency:.3f} ms", detail)
