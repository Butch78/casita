"""Correctness-gated mounted filesystem performance and isolation baseline."""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import math
import mmap
import os
from pathlib import Path
import platform
import random
import resource
import selectors
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time

from benchmarks import cli
from benchmarks.suites import repository as common

BOUNDARIES = (4095, 4096, 4097, 131071, 131072, 131073)


def require(condition, message):
    if not condition:
        raise common.BenchmarkError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def fixture(root, profile):
    root.mkdir()
    rng = random.Random(20260910)
    contents = {}
    sizes = [256] * (32 if profile == "smoke" else 2048)
    sizes += list(BOUNDARIES) + [8 * 1024**2 if profile == "smoke" else 64 * 1024**2]
    for index, size in enumerate(sizes):
        name = f"file-{index:05d}"
        contents[name] = rng.randbytes(size)
    # APFS rejects invalid UTF-8 source names. Native byte names are covered
    # separately by native-fskit and the native_mount integration test.
    byte_name = "byte-ascii" if platform.system() == "Darwin" else os.fsdecode(b"byte-\xff")
    contents[byte_name] = b"byte-safe name\n"
    echo = shutil.which("echo")
    require(echo is not None, "an echo executable is required for the execution gate")
    contents["echo"] = Path(echo).read_bytes()
    for name, data in contents.items():
        (root / name).write_bytes(data)
        (root / name).chmod(0o555 if name == "echo" else 0o444)
    os.symlink("file-00000", root / "link")
    os.symlink(byte_name, root / "byte-link")
    return contents, f"file-{len(sizes) - 1:05d}"


def build_server(output):
    command = ["cargo", "build", "-p", "casita-fs", "--release", "--all-features",
               "--example", "transport_server", "--message-format=json"]
    built = subprocess.run(command, cwd=cli.ROOT, capture_output=True, text=True)
    output.with_suffix(".build.log").write_text(built.stderr)
    require(built.returncode == 0, f"transport server build failed: {built.stderr[-4000:]}")
    for line in built.stdout.splitlines():
        row = json.loads(line)
        if row.get("target", {}).get("name") == "transport_server" and row.get("executable"):
            return Path(row["executable"])
    raise common.BenchmarkError("Cargo did not report a transport_server executable")


class Server:
    def __init__(self, binary, source, work, threads):
        work.mkdir()
        self.work = work
        self.log = (work / "server.log").open("w")
        self.process = subprocess.Popen([str(binary), str(source), str(work), str(threads)],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.log, start_new_session=True)
        self.buffer = b""
        try:
            self.ready = self.receive(180)
            require(self.ready.get("event") == "ready", "server failed its ready handshake")
            self.tree = Path(self.ready["tree"])
            self.mountpoint = Path(self.ready["mountpoint"])
            require(self.tree.is_dir() and os.path.ismount(self.mountpoint), "ready path is not in a mounted filesystem")
        except BaseException:
            self.abort()
            raise

    def receive(self, timeout=30):
        deadline = time.monotonic() + timeout
        with selectors.DefaultSelector() as selector:
            selector.register(self.process.stdout, selectors.EVENT_READ)
            while b"\n" not in self.buffer:
                left = deadline - time.monotonic()
                require(left > 0 and selector.select(left), "mount server response timed out")
                data = os.read(self.process.stdout.fileno(), 65536)
                require(bool(data), f"mount server exited; inspect {self.work / 'server.log'}")
                self.buffer += data
        line, self.buffer = self.buffer.split(b"\n", 1)
        return json.loads(line)

    def call(self, command, timeout=30):
        self.process.stdin.write(command.encode() + b"\n")
        self.process.stdin.flush()
        return self.receive(timeout)

    def close(self):
        stopped = self.call("stop")
        require(stopped.get("event") == "stopped", "server did not confirm teardown")
        require(self.process.wait(timeout=30) == 0, "server exited unsuccessfully")
        require(not os.path.ismount(self.mountpoint), "mount remains after server teardown")
        self.release_pipes()
        return stopped

    def release_pipes(self):
        self.process.stdin.close()
        self.process.stdout.close()
        self.log.close()

    def abort(self):
        # Only our own process group; retain work directories on any failure.
        if self.process.poll() is None:
            os.killpg(self.process.pid, signal.SIGTERM)
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(self.process.pid, signal.SIGKILL)
                self.process.wait()
        self.release_pipes()


def security_gates(tree, source):
    gates = {}
    file = tree / "file-00000"
    operations = {
        "write": lambda: file.write_bytes(b"forbidden"),
        "truncate": lambda: os.truncate(file, 0),
        "chmod": lambda: file.chmod(0o777),
        "unlink": file.unlink,
        "rename": lambda: file.rename(tree / "renamed"),
        "mkdir": lambda: (tree / "unauthorized").mkdir(),
        "symlink": lambda: os.symlink("file-00000", tree / "unauthorized-link"),
    }
    for name, operation in operations.items():
        try:
            operation()
        except OSError as error:
            # ENOSYS is not accepted: a runtime could silently emulate it.
            import errno
            require(error.errno in (errno.EROFS, errno.EACCES, errno.EPERM),
                    f"{name} returned unexpected error {error}")
            gates[name] = {"status": "passed", "errno": error.errno}
        else:
            raise common.BenchmarkError(f"read-only security gate failed: {name} succeeded")
    control = "import pathlib,sys; assert pathlib.Path(sys.argv[1]).read_bytes();\ntry: pathlib.Path(sys.argv[2]).read_bytes()\nexcept (PermissionError,FileNotFoundError): sys.exit(0)\nraise SystemExit(9)"
    if platform.system() == "Darwin":
        policy = f'(version 1)(allow default)(deny file-read* (subpath {json.dumps(str(tree.resolve()))}))'
        command = ["/usr/bin/sandbox-exec", "-p", policy, sys.executable, "-c", control]
    elif shutil.which("bwrap"):
        command = ["bwrap", "--die-with-parent", "--ro-bind", "/", "/", "--tmpfs", str(tree),
                   "--", sys.executable, "-c", control]
    else:
        gates["sandbox"] = {"status": "unavailable", "reason": "sandbox tool missing"}
        return gates
    result = subprocess.run([*command, str(source / "file-00000"), str(file)], capture_output=True, text=True, timeout=30)
    require(result.returncode == 0, f"sandbox positive-control/denial gate failed: {result.stderr}")
    gates["sandbox"] = {"status": "passed", "scope": "mount path denial with readable source control; not an IPC authentication proof"}
    return gates


def integrity(tree, contents):
    require(set(os.listdir(tree)) == set(contents) | {"link", "byte-link"}, "directory names differ")
    for name, expected in contents.items():
        path = tree / name
        require(path.read_bytes() == expected, f"content mismatch: {name!r}")
        require(stat.S_ISREG(path.stat().st_mode), f"wrong file kind: {name!r}")
        require(bool(path.stat().st_mode & 0o111) == (name == "echo"), f"wrong executable bit: {name!r}")
    require(os.readlink(tree / "link") == "file-00000", "symlink target differs")
    require(os.fsencode(os.readlink(tree / "byte-link")) == (b"byte-ascii" if "byte-ascii" in contents else b"byte-\xff"), "byte symlink target differs")
    require(subprocess.check_output([str(tree / "echo"), "casita-exec"], timeout=10) == b"casita-exec\n", "execution failed")
    for name in contents:
        with (tree / name).open("rb") as stream, mmap.mmap(stream.fileno(), 0, access=mmap.ACCESS_READ) as mapped:
            require(mapped[:] == contents[name], f"mmap mismatch: {name!r}")


def sample(operation, implementation, phase, repetition, tasks, function, concurrency=1, server=None):
    before = server.call("stats") if server else None
    usage_before = resource.getrusage(resource.RUSAGE_SELF)
    host_load_before = os.getloadavg()
    def measured(task):
        start = time.perf_counter_ns()
        count = function(task)
        return time.perf_counter_ns() - start, count
    started = time.perf_counter_ns()
    if concurrency == 1:
        observations = [measured(task) for task in tasks]
    else:
        with ThreadPoolExecutor(max_workers=concurrency) as pool:
            observations = list(pool.map(measured, tasks))
    elapsed = time.perf_counter_ns() - started
    usage_after = resource.getrusage(resource.RUSAGE_SELF)
    nanos = sorted(value[0] for value in observations)
    quantile = lambda p: nanos[min(len(nanos) - 1, max(0, math.ceil(p * len(nanos)) - 1))]
    count = sum(value[1] for value in observations)
    after = server.call("stats") if server else None
    return dict(status="ok", operation=operation, implementation=implementation, phase=phase,
        repetition=repetition, concurrency=concurrency, operations=len(tasks), bytes=count,
        wall_seconds=elapsed / 1e9, throughput_bytes_per_second=count / (elapsed / 1e9),
        p50_nanos=quantile(.5), p95_nanos=quantile(.95), p99_nanos=quantile(.99), max_nanos=nanos[-1],
        latencies_nanos=[value[0] for value in observations], task_inputs=list(tasks),
        bytes_per_operation=[value[1] for value in observations], correctness="passed",
        client_cpu_seconds=(usage_after.ru_utime + usage_after.ru_stime - usage_before.ru_utime - usage_before.ru_stime),
        client_max_rss_bytes=usage_after.ru_maxrss * (1 if platform.system() == "Darwin" else 1024),
        host_load_before=host_load_before, host_load_after=os.getloadavg(),
        server_before=before, server_after=after)


def run_cases(tree, contents, bulk, profile, implementation, repetition, server=None):
    names = sorted(contents)
    hashes = {name: digest(data) for name, data in contents.items()}
    reads = 16 if profile == "smoke" else 256
    rng = random.Random(42)
    offsets = [rng.randrange(len(contents[bulk]) - 4096) for _ in range(reads)]
    def metadata(name):
        require((tree / name).stat().st_size == len(contents[name]), "stat size differs")
        return 0
    def listing(_):
        require(set(os.listdir(tree)) == set(contents) | {"link", "byte-link"}, "listing differs")
        return 0
    def read(name):
        data = (tree / name).read_bytes()
        require(digest(data) == hashes[name], "read digest differs")
        return len(data)
    def random_read(offset):
        with (tree / bulk).open("rb") as file:
            data = os.pread(file.fileno(), 4096, offset)
        require(data == contents[bulk][offset:offset+4096], "random read differs")
        return len(data)
    def mapped_read(_):
        with (tree / bulk).open("rb") as file, mmap.mmap(file.fileno(), 0, access=mmap.ACCESS_READ) as data:
            require(hashlib.sha256(data).hexdigest() == hashes[bulk], "mapped digest differs")
        return len(contents[bulk])
    def execute(_):
        require(subprocess.check_output([str(tree / "echo"), "casita-exec"], timeout=10) == b"casita-exec\n", "execution differs")
        return 0
    rows = []
    for phase in ("first-pass", "repeat-pass"):
        for operation, tasks, function in [
            ("stat", names, metadata), ("readdir", list(range(reads)), listing),
            ("small-and-boundary-reads", [n for n in names if n not in (bulk, "echo")], read),
            ("sequential-read", [bulk] * 3, read), ("mmap", [None] * 3, mapped_read),
            ("exec", [None] * (4 if profile == "smoke" else 32), execute),
        ]:
            rows.append(sample(operation, implementation, phase, repetition, tasks, function, server=server))
        for concurrency in ((1, 4) if profile == "smoke" else (1, 4, 16)):
            rows.append(sample("random-open-read", implementation, phase, repetition, offsets, random_read, concurrency, server))
    return rows


def cached_read_controls(tree, contents, bulk, implementation, repetition, server=None):
    """After both passes: distinguish path opens, copying and client overhead."""
    names = sorted(n for n in contents if n not in (bulk, "echo"))
    paths = {name: tree / name for name in names}
    hashes = {name: digest(contents[name]) for name in names}
    def open_close(name):
        fd = os.open(paths[name], os.O_RDONLY)
        os.close(fd)
        return 0
    def posix_read(name):
        fd = os.open(paths[name], os.O_RDONLY)
        try:
            data = os.pread(fd, len(contents[name]) + 1, 0)
        finally:
            os.close(fd)
        require(digest(data) == hashes[name], "POSIX read digest differs")
        return len(data)
    def pathlib_read(name):
        data = paths[name].read_bytes()
        require(digest(data) == hashes[name], "pathlib read digest differs")
        return len(data)
    def memory_hash(name):
        require(digest(contents[name]) == hashes[name], "memory digest differs")
        return len(contents[name])
    hot_name = "file-00000"
    rows = []
    for trial in range(3):
        phase = f"cached-control-{trial}"
        for operation, function in [("cached-pathlib-read", pathlib_read),
                                    ("cached-open-close", open_close),
                                    ("cached-posix-read", posix_read),
                                    ("memory-hash-control", memory_hash)]:
            rows.append(sample(operation, implementation, phase, repetition, names, function, server=server))
        # Same 256-byte file and verification on both sides of the fd-reuse comparison.
        tasks = [hot_name] * len(names)
        rows.append(sample("hot-file-open-read", implementation, phase, repetition, tasks, posix_read, server=server))
        fd = os.open(paths[hot_name], os.O_RDONLY)
        try:
            def held_read(name):
                data = os.pread(fd, len(contents[name]) + 1, 0)
                require(digest(data) == hashes[name], "held-fd read digest differs")
                return len(data)
            rows.append(sample("hot-file-held-fd-read", implementation, phase, repetition, tasks, held_read, server=server))
        finally:
            os.close(fd)
    return rows


def build_parser():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="smoke")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--server-binary", type=Path)
    parser.add_argument("--host-only", action="store_true", help="explicitly collect only a host filesystem control")
    parser.add_argument("--server-threads", type=int, default=1)
    parser.add_argument("--measurement-note", default="", help="record external load or other run limitations")
    parser.add_argument("--output", type=Path, required=True)
    return parser


def main(argv=None):
    parser = build_parser()
    args = parser.parse_args(argv)
    if min(args.repetitions, args.server_threads) < 1:
        parser.error("repetitions and threads must be positive")
    args.output = args.output.resolve()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="casita-transports-"))
    result = dict(schema_version=1, result_schema="casita.filesystem-transports.v1", suite_id="repository-e2e",
        complete=False, security_complete=False, decision_eligible=False, environment=common.environment_metadata(work),
        configuration=dict(profile=args.profile, repetitions=args.repetitions, server_threads=args.server_threads,
            raw_byte_source_names=platform.system() != "Darwin",
            measurement_note=args.measurement_note,
            host_only=args.host_only, sizes=list(BOUNDARIES), seed=20260910,
            cache_policy="first/repeat passes; no cache eviction, import warms backing storage; not a cold-disk measurement",
            validation_in_timing=True, order="host/mount order alternates across repetitions"),
        work_directory=str(work), samples=[], security=[], sessions=[], direct_reader=[])
    save = lambda: common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
    save()
    active = []
    try:
        contents, bulk = fixture(work / "source", args.profile)
        source = work / "source"
        result["fixture"] = {os.fsencode(name).hex(): {"size": len(data), "sha256": digest(data)} for name, data in contents.items()}
        binary = None if args.host_only else (args.server_binary or build_server(args.output)).resolve()
        if binary:
            shutil.copy2(binary, work / "transport_server")
            binary = work / "transport_server"
            result["server_binary"] = {"path": str(binary), "sha256": digest(binary.read_bytes())}
            result["source_sha256"] = {str(path.relative_to(cli.ROOT)): digest(path.read_bytes())
                for path in [cli.ROOT / "Cargo.toml", cli.ROOT / "crates/casita/Cargo.toml", cli.ROOT / "Cargo.lock",
                             cli.ROOT / "benchmarks/suites/filesystem_transports.py",
                             cli.ROOT / "crates/casita-fs/Cargo.toml", cli.ROOT / "crates/casita-fs/examples/transport_server.rs",
                             *sorted((cli.ROOT / "crates/casita/src").rglob("*.rs")),
                             *sorted((cli.ROOT / "crates/casita-fs/src").rglob("*.rs"))]}
        for repetition in range(args.repetitions):
            order = ["host"] if args.host_only else (["host", "mount"] if repetition % 2 == 0 else ["mount", "host"])
            for target in order:
                print(f"filesystem-transports: {target}, repetition {repetition}", flush=True)
                if target == "host":
                    rows = run_cases(source, contents, bulk, args.profile, "host", repetition)
                    rows.extend(cached_read_controls(source, contents, bulk, "host", repetition))
                    integrity(source, contents)
                    result["samples"].extend(rows)
                else:
                    server = Server(binary, source, work / f"mount-{repetition}", args.server_threads)
                    active.append(server)
                    rows = run_cases(server.tree, contents, bulk, args.profile, server.ready["transport"], repetition, server)
                    rows.extend(cached_read_controls(server.tree, contents, bulk, server.ready["transport"], repetition, server))
                    expected_sizes = sorted(len(data) for name, data in contents.items() if name not in (bulk, "echo"))
                    # Alternate order: either API can warm shared storage/cache state.
                    direct_modes = ["direct-reads", "direct-retained-reads"]
                    for mode in direct_modes[::1 if repetition % 2 == 0 else -1]:
                        direct = server.call(mode, timeout=180)
                        require(direct.get("event") == mode, "direct reader control failed")
                        require(len(direct["passes"]) == 2, "missing direct reader pass")
                        for direct_pass in direct["passes"]:
                            require(direct_pass["correctness"] == "passed" and
                                    sorted(row["bytes"] for row in direct_pass["observations"]) == expected_sizes,
                                    "direct reader did not cover the mounted workload")
                        result["direct_reader"].append({"repetition": repetition, **direct})
                    integrity(server.tree, contents)
                    gates = security_gates(server.tree, source)
                    other_source = work / f"other-source-{repetition}"
                    other_source.mkdir()
                    (other_source / "canary").write_bytes(b"independent mount\n")
                    other = Server(binary, other_source, work / f"other-mount-{repetition}", args.server_threads)
                    active.append(other)
                    require((other.tree / "canary").read_bytes() == b"independent mount\n", "second mount read failed")
                    require(not (server.tree / "canary").exists() and not (other.tree / "file-00000").exists(),
                            "independent mounts share a namespace")
                    other.close()
                    active.remove(other)
                    require((server.tree / "file-00000").read_bytes() == contents["file-00000"],
                            "stopping the second mount broke the first")
                    gates["independent_mounts"] = {"status": "passed"}
                    # Check a stale-negative lookup and a cached listing before publication.
                    if platform.system() == "Darwin":
                        pending = server.tree.parent / "published"
                        require(not pending.exists(), "publication fixture already exists")
                        list(server.tree.parent.iterdir())
                        start = time.perf_counter_ns()
                        published = server.call("publish")
                        publish_nanos = time.perf_counter_ns() - start
                        require(Path(published["path"]) == pending, "unexpected publication path")
                        integrity(pending, contents)
                        gates["publication"] = {"status": "passed", "latency_nanos": publish_nanos}
                    integrity(server.tree, contents)
                    stopped = server.close()
                    active.remove(server)
                    result["sessions"].append({"repetition": repetition, "ready": server.ready, "stopped": stopped})
                    result["security"].append({"repetition": repetition, "gates": gates})
                    result["samples"].extend(rows)
                save()
        require(result["samples"], "no benchmark samples")
        if binary:
            require(digest(binary.read_bytes()) == result["server_binary"]["sha256"], "benchmark binary changed")
        result["complete"] = True
        result["security_complete"] = bool(result["security"]) and all(
            gate["status"] == "passed" for run in result["security"] for gate in run["gates"].values())
        # Baselines are not a security certification or a candidate comparison.
        result["decision_eligible"] = False
        save()
    except BaseException as error:
        result["error"] = str(error) or type(error).__name__
        for row in result["samples"]:
            row["status"] = "invalid"
        save()
        raise
    finally:
        for server in active:
            server.abort()
    print(f"results: {args.output}; fixtures and logs retained at {work}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
