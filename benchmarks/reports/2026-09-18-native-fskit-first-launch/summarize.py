"""Verify first-launch controls and print phase and reader-cache comparisons."""
import json
from pathlib import Path
from statistics import median

ROOT = Path(__file__).resolve().parent
BACKENDS = ("native-casita-repository", "fuser-casita-repository", "host")


def summarize(report, label):
    assert report["complete"] and report["comparison_complete"] and report["first_launch_complete"]
    assert not report["cleanup_errors"]
    rows = report["first_launch_trials"]
    expected = {(r, mode, target, backend) for r in range(3)
                for mode in ("none", "read", "static-code") for target in ("script", "native") for backend in BACKENDS}
    assert len(rows) == len(expected)
    assert {(r["repetition"], r["preparation"], r["target"], r["implementation"]) for r in rows} == expected
    assert all(r["correctness"] == r["teardown"] == "passed" for r in rows)
    assert len({r["tree"] for r in rows}) == len(rows)
    print("\n", label)
    for mode in ("none", "read", "static-code"):
        for target in ("script", "native"):
            for backend in BACKENDS:
                rows = [r for r in report["first_launch_trials"] if
                        (r["preparation"], r["target"], r["implementation"]) == (mode, target, backend)]
                phases = {"setup": median(r["setup_ns"] for r in rows),
                          "prepare": median(r["prepare"]["total_ns"] for r in rows),
                          "first": median(r["first"]["total_ns"] for r in rows),
                          "spawn": median(r["first"]["spawn_ns"] for r in rows),
                          "prepare+first": median(r["prepare"]["total_ns"] + r["first"]["total_ns"] for r in rows),
                          "second": median(r["second"]["total_ns"] for r in rows)}
                counters = {key: median(r["counters_first"].get(key, 0)-r["counters_prepared"].get(key, 0) for r in rows)
                            for key in ("blob_opens", "reads", "open_ns", "directories", "directory_ns")}
                print(mode, target, backend, {k: round(v/1e6, 3) for k, v in phases.items()}, counters)
    for row in report.get("reader_cache_pressure", []):
        assert row["correctness"] == row["repository_release_barrier"] == "passed"
        print("reader pressure", row["width"], "files", row["cycles"], "cycles",
              round(row["elapsed_ns"]/1e6, 3), "ms", row["after"]["native_reader_cache"],
              "opens", row["after"]["blob_opens"]-row["before"]["blob_opens"])


if __name__ == "__main__":
    initial = json.loads((ROOT / "first-launch-512.json").read_text())
    summarize(initial, "initial investigation")
    reports = [json.loads((ROOT / f"first-launch-readers-{mode}.json").read_text())
               for mode in ("disabled", "enabled")]
    for key in ("source_sha256", "build_identity", "binaries", "server_sha256", "snapshot"):
        assert reports[0][key] == reports[1][key], key
    for mode, report in zip(("disabled", "enabled"), reports):
        assert report["configuration"]["native_reader_cache"] == mode
        assert [row["width"] for row in report["reader_cache_pressure"]] == [15, 16, 17]
        for row in report["first_launch_trials"]:
            if row["implementation"] == "native-casita-repository":
                assert row["counters_before"]["native_reader_cache"]["enabled"] == (mode == "enabled")
                assert row["native_teardown"]["stats"]["native_reader_cache"]["resident"] == 0
        summarize(report, mode)
