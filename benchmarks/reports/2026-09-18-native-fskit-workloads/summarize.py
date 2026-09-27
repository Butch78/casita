"""Check matching real-tool receipts and summarize concurrent batch latency."""
import json
from pathlib import Path
from statistics import median

ROOT = Path(__file__).resolve().parent
BACKENDS = ("native-casita-repository", "fuser-casita-repository", "host")
COUNTS = (1, 8, 15, 16, 17)


def summarize(report, mode):
    assert report["complete"] and report["comparison_complete"] and report["workloads_complete"]
    assert not report["cleanup_errors"]
    rows = report["workload_trials"]
    expected = {(r, p, n, b) for r in range(3) for p in ("shared", "distinct") for n in COUNTS for b in BACKENDS}
    assert len(rows) == len(expected)
    assert {(r["repetition"], r["pattern"], r["workers"], r["implementation"]) for r in rows} == expected
    assert len({r["tree"] for r in rows}) == len(rows)
    for row in rows:
        assert row["correctness"] == row["teardown"] == "passed"
        for phase in ("first", "repeat"):
            assert len(row[phase]["samples"]) == row["workers"]
            assert all(s["correctness"] == "passed" for s in row[phase]["samples"])
        if row["implementation"] == BACKENDS[0]:
            assert row["counters_before"]["native_reader_cache"]["enabled"] == (mode == "enabled")
            assert row["native_teardown"]["stats"]["native_reader_cache"]["resident"] == 0
    print("\n", mode)
    for pattern in ("shared", "distinct"):
        for count in COUNTS:
            for backend in BACKENDS:
                selected = [r for r in rows if (r["pattern"],r["workers"],r["implementation"]) == (pattern,count,backend)]
                first = median(r["first"]["wall_ns"] for r in selected)/1e6
                repeat = median(r["repeat"]["wall_ns"] for r in selected)/1e6
                opens = [r["counters_first"].get("blob_opens", 0)-r["counters_before"].get("blob_opens", 0) for r in selected]
                reads = [r["counters_first"].get("reads", 0)-r["counters_before"].get("reads", 0) for r in selected]
                print(pattern, count, backend, f"first={first:.3f} ms repeat={repeat:.3f} ms",
                      "opens", opens, "reads", reads)


if __name__ == "__main__":
    reports = [json.loads((ROOT / f"workloads-{mode}.json").read_text()) for mode in ("disabled", "enabled")]
    for key in ("source_sha256", "build_identity", "binaries", "server_sha256", "snapshot", "workload_fixture"):
        assert reports[0][key] == reports[1][key], key
    for mode, report in zip(("disabled", "enabled"), reports):
        assert report["configuration"]["native_reader_cache"] == mode
        summarize(report, mode)
