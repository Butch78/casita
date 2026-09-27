"""Verify density receipts and summarize public bundle/listing controls."""
import json
from pathlib import Path
from statistics import median

ROOT = Path(__file__).resolve().parent
CASES = ("native-listdir", "native-bundle-discovery", "native-static-code",
         "script-timeout", "native-timeout")


def summarize():
    reference = None
    for count in (0, 128, 256, 512):
        report = json.loads((ROOT / f"directory-controls-{count}.json").read_text())
        assert report["complete"] and report["comparison_complete"] and not report["cleanup_errors"]
        identity = [report[k] for k in ("source_sha256", "build_identity", "binaries", "server_sha256")]
        if reference is not None:
            assert identity == reference
        reference = identity
        assert report["configuration"]["metadata_files"] == count
        print(f"\nMetadata siblings: {count}")
        for case in CASES:
            for backend in ("native-casita-repository", "fuser-casita-repository", "host"):
                rows = [r for r in report["samples"] if r["case"] == case and r["implementation"] == backend]
                assert len(rows) == 3 and all(r["correctness"] == "passed" for r in rows)
                counters = {k: [] for k in ("callbacks", "packed", "directories", "enum_ms")}
                for row in rows:
                    for sample in row["phases"]:
                        before, after = (sample[k] for k in ("counters_before", "counters_after"))
                        for key, index in (("callbacks", 0), ("packed", 1), ("enum_ms", 2)):
                            counters[key].append(sum(v[index] for v in after.get("native_enumerations", {}).values())
                                                 - sum(v[index] for v in before.get("native_enumerations", {}).values()))
                        counters["directories"].append(after.get("directories", 0)-before.get("directories", 0))
                values = {k: median(v) for k, v in counters.items()}
                values["enum_ms"] /= 1e6
                print(case, backend, f'{median(r["p50_ns"] for r in rows)/1e6:.3f} ms', values)


def timestamps():
    for prefix, counts in (("timestamps", (None,)), ("timestamps-density", (0, 128, 256, 512))):
        for count in counts:
            modes = ("zero", "store") if count is None else ("zero", "default")
            paths = [ROOT / (f"{prefix}-{mode}.json" if count is None else
                             f"timestamps-{mode}-density-{count}.json") for mode in modes]
            reports = [json.loads(path.read_text()) for path in paths]
            for key in ("source_sha256", "build_identity", "binaries", "server_sha256", "snapshot"):
                assert reports[0][key] == reports[1][key], (paths, key)
            for mode, report in zip(modes, reports):
                assert report["complete"] and report["comparison_complete"] and not report["cleanup_errors"]
                expected = 0 if mode == "zero" else 1_000_000_000
                assert all(v == expected for v in report["item_timestamps"]["native-casita-repository"].values())
                print("\nTIMESTAMPS", count if count is not None else 512, mode)
                for case in ("script-timeout", "native-timeout", "native-static-code"):
                    for backend in ("native-casita-repository", "fuser-casita-repository", "host"):
                        rows = [r for r in report["samples"] if r["case"] == case and r["implementation"] == backend]
                        assert len(rows) == 3
                        calls = []
                        for row in rows:
                            for sample in row["phases"]:
                                before, after = (sample[k].get("native_enumerations", {}) for k in ("counters_before", "counters_after"))
                                calls.append(sum(v[0] for v in after.values())-sum(v[0] for v in before.values()))
                        print(case, backend, f'{median(r["p50_ns"] for r in rows)/1e6:.3f} ms',
                              f"enum min/median/max={min(calls)}/{median(calls):g}/{max(calls)}"
                              if backend == "native-casita-repository" else "native enumeration counter: not applicable")


if __name__ == "__main__":
    summarize()
    timestamps()
