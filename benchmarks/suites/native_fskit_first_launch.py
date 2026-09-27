"""Fresh-mount first execution, preparation and immediate repeat controls."""
import itertools
import json
import os
import shutil
import time

from benchmarks.suites import native_fskit_launch as launch
from benchmarks.suites.filesystem_transports import require

PREPARATIONS = ("none", "read", "static-code")
TARGETS = (("script", "run"), ("native", "native-executable"))


def prepare(tree, filename, mode, expected):
    if mode == "none":
        return {"total_ns": 0}
    if mode == "static-code":
        return launch.static_code_query(tree, filename)
    require(mode == "read", "unknown first-launch preparation")
    started = time.perf_counter_ns()
    data = (tree / filename).read_bytes()
    elapsed = time.perf_counter_ns() - started
    require(data == expected, "first-launch preparation bytes differ")
    return {"total_ns": elapsed, "bytes": len(data)}


def validate(rows, repetitions, backends):
    expected = {(r, p, target, b) for r in range(repetitions)
                for p in PREPARATIONS for target, _ in TARGETS for b in backends}
    keys = [(row["repetition"], row["preparation"], row["target"], row["implementation"]) for row in rows]
    require(len(keys) == len(expected) and set(keys) == expected, "first-launch trials missing or duplicated")
    require(len({row["tree"] for row in rows}) == len(rows), "first-launch trials reused a path")
    require(all(row["correctness"] == "passed" and row["teardown"] == "passed" for row in rows),
            "first-launch correctness or teardown incomplete")


def run_trials(run, server_binary, work, files, repetitions, result, save):
    from benchmarks.suites.native_fskit_repository import NATIVE, correctness
    backends = (NATIVE, "host")
    rows = result["first_launch_trials"] = []
    result["first_launch_policy"] = {
        "cache": "fresh mount or fresh host file identity; shared OS and backing-store caches are not purged",
        "preparations": PREPARATIONS,
        "mount_setup": "native mount command; host file copy",
        "validation": "execution output checked immediately; complete mounted byte/listing oracle after both launches",
    }
    sequence = itertools.count()
    cases = tuple(itertools.product(PREPARATIONS, TARGETS))
    for repetition in range(repetitions):
        order = backends[repetition % len(backends):] + backends[:repetition % len(backends)]
        for mode, (target, filename) in cases[repetition % len(cases):] + cases[:repetition % len(cases)]:
            for backend in order:
                label = f"first-launch-{next(sequence):03d}"
                setup_started = time.perf_counter_ns()
                mount = None
                if backend == NATIVE:
                    mount = run.mount_repository(work / "repository", label)
                    tree = mount / "views/fixture"
                    counter_ids = itertools.count()
                    def stats():
                        return json.loads((mount / f"__casita_stats-{next(counter_ids)}").read_text())
                else:
                    tree = work / label
                    shutil.copytree(work / "source", tree, symlinks=True)
                    def stats():
                        return {}
                setup_ns = time.perf_counter_ns() - setup_started
                row = {"repetition": repetition, "preparation": mode, "target": target,
                       "implementation": backend, "tree": str(tree), "setup_ns": setup_ns,
                       "load": os.getloadavg(), "correctness": "pending", "teardown": "pending"}
                rows.append(row)
                save()
                try:
                    row["counters_before"] = stats()
                    row["prepare"] = prepare(tree, filename, mode, files[filename])
                    row["counters_prepared"] = stats()
                    row["first"] = launch.launch(tree, f"{target}-timeout")
                    row["counters_first"] = stats()
                    row["second"] = launch.launch(tree, f"{target}-timeout")
                    row["counters_second"] = stats()
                    # Gates run after measurement so they cannot prewarm either launch.
                    correctness(tree, work / "source", files)
                    row["correctness"] = "passed"
                finally:
                    if mount is not None:
                        run.unmount(mount)
                        row["native_teardown"] = json.loads((work / "repository/native-final.json").read_text())
                        require(row["native_teardown"]["repository_release_barrier"] == "passed",
                                "first-launch native reader release failed")
                    row["teardown"] = "passed"
                    save()
    validate(rows, repetitions, backends)
    result["first_launch_complete"] = True


def main(argv=None):
    import sys
    from benchmarks.suites.native_fskit_repository import main as repository_main
    return repository_main(["--launch-only", "--first-launch", *(sys.argv[1:] if argv is None else argv)])


if __name__ == "__main__":
    raise SystemExit(main())
