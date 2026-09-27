"""Real command-line tools on fresh mounts, including concurrent cache pressure."""
from concurrent.futures import ThreadPoolExecutor
import gzip
import hashlib
import itertools
import json
import os
from pathlib import Path
import shutil
import subprocess
import threading
import time

from benchmarks.suites.filesystem_transports import require

COUNTS = (1, 8, 15, 16, 17, 31, 32, 33)
PATTERNS = ("shared", "distinct")
TOOLS = ("awk", "sort", "gzip")
PAYLOAD = b"".join(f"{i:08d}\n".encode() for i in reversed(range(16384)))
SORTED = b"".join(sorted(PAYLOAD.splitlines(keepends=True)))
NUMBERED = b"".join(str(i).encode() + b":" + line for i, line in enumerate(PAYLOAD.splitlines(keepends=True), 1))


def check_output(tool, stdout):
    if tool == "gzip":
        require(gzip.decompress(stdout) == PAYLOAD, "gzip round-trip differs")
    else:
        require(stdout == (NUMBERED if tool == "awk" else SORTED), f"{tool} output differs")


def execute(path, tool, gate=None):
    # Preserve the command name for distributions using multicall binaries.
    argv = [tool, '{print NR ":" $0}'] if tool == "awk" else [tool, "-c"] if tool == "gzip" else [tool]
    environment = {**os.environ, "LC_ALL": "C", "LANG": "C"}
    if gate:
        gate.wait(timeout=60)
    started = time.perf_counter_ns()
    process = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE, env=environment, executable=str(path))
    spawned = time.perf_counter_ns()
    try:
        stdout, stderr = process.communicate(PAYLOAD, timeout=120)
    except BaseException:
        process.kill()
        process.communicate()
        raise
    finished = time.perf_counter_ns()
    require(process.returncode == 0, f"{tool} exited {process.returncode}: {stderr!r}")
    check_output(tool, stdout)
    return {"tool":tool, "path":str(path), "total_ns":finished-started,
            "spawn_ns":spawned-started, "finish_ns":finished, "correctness":"passed"}


def add_fixture(root, files):
    receipts = {}
    contents = {}
    for tool in TOOLS:
        selected = os.environ.get(f"CASITA_WORKLOAD_{tool.upper()}") or shutil.which("gawk" if tool == "awk" else tool)
        require(selected is not None, f"set CASITA_WORKLOAD_{tool.upper()} to a relocatable executable")
        source = Path(selected).resolve()
        data = source.read_bytes()
        require(not data.startswith(b"#!"), f"select the underlying executable for {tool}; wrapper scripts may execute host files")
        contents[tool] = data
        receipts[tool] = {"source":str(source), "sha256":hashlib.sha256(data).hexdigest(), "size":len(data)}
    for name, tool in [("workload-shared", "awk"), *[(f"workload-{i:02d}", TOOLS[i % len(TOOLS)]) for i in range(max(COUNTS))]]:
        files[name] = contents[tool]
        (root / name).write_bytes(contents[tool])
        (root / name).chmod(0o555)
    # Check copied executables before import. Shared libraries remain on the host.
    for i, tool in enumerate(TOOLS):
        execute(root / f"workload-{i:02d}", tool)
    return {"tools":receipts, "payload_sha256":hashlib.sha256(PAYLOAD).hexdigest(), "payload_bytes":len(PAYLOAD)}


def batch(tree, pattern, count):
    released = []
    gate = threading.Barrier(count + 1, action=lambda: released.append(time.perf_counter_ns()))
    with ThreadPoolExecutor(max_workers=count) as pool:
        futures = [pool.submit(execute,
                    tree / ("workload-shared" if pattern == "shared" else f"workload-{i:02d}"),
                    "awk" if pattern == "shared" else TOOLS[i % len(TOOLS)], gate)
                   for i in range(count)]
        gate.wait(timeout=60)
        samples = [future.result(timeout=150) for future in futures]
    return {"wall_ns":max(row["finish_ns"] for row in samples)-released[0], "samples":samples}


def validate(rows, repetitions, backends, counts=COUNTS):
    expected = {(r, p, n, b) for r in range(repetitions) for p in PATTERNS for n in counts for b in backends}
    keys = [(r["repetition"], r["pattern"], r["workers"], r["implementation"]) for r in rows]
    require(len(keys) == len(expected) and set(keys) == expected, "workload matrix missing or duplicated")
    require(len({row["tree"] for row in rows}) == len(rows), "workload reused a fresh path")
    for row in rows:
        require(row["correctness"] == row["teardown"] == "passed", "workload gates incomplete")
        for phase in ("first", "repeat"):
            require(len(row[phase]["samples"]) == row["workers"] and
                    all(s["correctness"] == "passed" for s in row[phase]["samples"]), "workload sample incomplete")


def run_trials(run, binary, work, files, repetitions, result, save):
    from benchmarks.suites.native_fskit_repository import NATIVE, correctness
    backends = (NATIVE, "host")
    counts = tuple(result["configuration"].get("workload_workers") or COUNTS)
    rows = result["workload_trials"] = []
    result["workload_policy"] = {
        "workers":counts, "patterns":PATTERNS,
        "cache":"fresh mount or fresh host inode, then immediate repeat; shared OS/backing caches not purged",
        "input":"identical host-provided stdin; executable files mounted, shared libraries remain on host",
        "distinct":"distinct file identities; tools repeat awk/sort/gzip, not necessarily different applications",
        "wall":"barrier release through last child completion, excludes output validation and thread setup",
    }
    sequence = itertools.count()
    cases = tuple(itertools.product(PATTERNS, counts))
    for repetition in range(repetitions):
        for pattern, count in cases[repetition % len(cases):] + cases[:repetition % len(cases)]:
            for backend in backends[repetition % len(backends):] + backends[:repetition % len(backends)]:
                label = f"workloads-{next(sequence):03d}"
                mount = None
                started = time.perf_counter_ns()
                if backend == NATIVE:
                    mount = run.mount_repository(work / "repository", label)
                    tree = mount / "views/fixture"
                    ids = itertools.count()
                    def stats():
                        return json.loads((mount / f"__casita_stats-{next(ids)}").read_text())
                else:
                    tree = work / label
                    shutil.copytree(work / "source", tree, symlinks=True)
                    def stats():
                        return {}
                row = {"repetition":repetition, "pattern":pattern, "workers":count, "implementation":backend,
                       "tree":str(tree), "setup_ns":time.perf_counter_ns()-started, "load":os.getloadavg(),
                       "correctness":"pending", "teardown":"pending"}
                rows.append(row)
                save()
                try:
                    row["counters_before"] = stats()
                    row["first"] = batch(tree, pattern, count)
                    row["counters_first"] = stats()
                    row["repeat"] = batch(tree, pattern, count)
                    row["counters_repeat"] = stats()
                    if backend == NATIVE and result["configuration"].get("native_trace_read_ranges"):
                        for phase in ("before", "first", "repeat"):
                            counters = row[f"counters_{phase}"]
                            trace = counters["native_read_trace"]
                            require(trace["enabled"] and trace["dropped"] == 0, "read trace disabled or truncated")
                            require(sum(v[0] for v in trace["ranges"].values()) == counters["reads"],
                                    "read trace does not cover every callback")
                            require(all(v[3] == 0 for v in trace["ranges"].values()), "traced read failed")
                            callbacks = trace["callbacks"]
                            require(len(callbacks) == 5 and 0 <= callbacks[0] <= counters["reads"]
                                    and callbacks[4] == 0, "invalid FSKit callback trace")
                    correctness(tree, work / "source", files)
                    row["correctness"] = "passed"
                finally:
                    if mount:
                        run.unmount(mount)
                        row["native_teardown"] = json.loads((work / "repository/native-final.json").read_text())
                        require(row["native_teardown"]["repository_release_barrier"] == "passed", "workload release failed")
                        if result["configuration"].get("native_trace_read_ranges"):
                            final = row["native_teardown"]["stats"]
                            require(final["native_read_trace"]["callbacks"][0] == final["reads"],
                                    "FSKit callback trace incomplete after unmount")
                    row["teardown"] = "passed"
                    save()
    validate(rows, repetitions, backends, counts)
    result["workloads_complete"] = True


def main(argv=None):
    import sys
    from benchmarks.suites.native_fskit_repository import main as repository_main
    return repository_main(["--launch-only", "--workloads", *(sys.argv[1:] if argv is None else argv)])


if __name__ == "__main__":
    raise SystemExit(main())
