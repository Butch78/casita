"""Compare native Rust FSKit repository reads with the host filesystem."""
from __future__ import annotations
import argparse
import hashlib
import itertools
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import subprocess
import tempfile
import time

from benchmarks import cli
from benchmarks.suites import native_fskit as memory
from benchmarks.suites import repository as common
from benchmarks.suites.filesystem_transports import require

NATIVE = "native-casita-repository"
EXECUTION_CASES = ("execute-script", "execute-native")


def deadline_expired(_signal, _frame):
    raise common.BenchmarkError("timed comparison exceeded its deadline")


def execute(tree, case):
    args = [str(tree / "run")] if case == "execute-script" else [str(tree / "native-executable")]
    expected = b"casita-native-fskit-ok\n" if case == "execute-script" else b"native-ok\n"
    start = time.perf_counter_ns()
    # Preserve first execution, including code-signature reads, separately.
    command = subprocess.run(args, capture_output=True, timeout=120)
    require(command.returncode == 0 and command.stdout == expected,
            f"{case} failed: exit={command.returncode}, stderr={command.stderr!r}")
    return time.perf_counter_ns() - start

class RepositoryRun(memory.NativeRun):
    def mount_repository(self, repository, label):
        root = self.work / label
        root.mkdir()
        self.mounts.append(root)
        self.mount_command(["/sbin/mount", "-F", "-t", "casitarepo", repository, root])
        require(os.path.ismount(root), "native repository mount missing")
        self.result["rootless_mount"] = True
        return root


def fixture(root, tag, executable=None, metadata_files=256):
    files = memory.fixture(portable=True)
    files = {name: data for name, data in files.items() if not name.startswith("meta-")}
    files.update({f"meta-{i:04}": f"metadata-{i}\n".encode() for i in range(metadata_files)})
    files["namespace"] = tag.encode()
    files["native-executable"] = Path(executable or shutil.which("echo") or "/bin/echo").read_bytes()
    memory.materialize(root, files)
    (root / "native-executable").chmod(0o555)
    (root / "nested/child").mkdir(parents=True)
    (root / "nested/child/payload").write_bytes(b"nested repository data\n" + tag.encode())
    return files


def correctness(tree, source, files):
    expected = set(files) | {"link", "nested"}
    listed = os.listdir(tree)
    require(len(listed) == len(expected) and set(listed) == expected, "repository root names differ or repeat")
    for name, data in files.items():
        path = tree / name
        require(path.read_bytes() == data, f"repository bytes differ: {name}")
        require(path.stat().st_size == len(data), f"repository size differs: {name}")
        with path.open("rb") as handle:
            for offset in (0, max(0, len(data)-1), len(data), len(data)+1):
                require(os.pread(handle.fileno(), 17, offset) == data[offset:offset+17], "range/EOF differs")
            if data:
                with memory.mmap.mmap(handle.fileno(), 0, access=memory.mmap.ACCESS_READ) as mapped:
                    require(mapped[:] == data, "mmap differs")
    require((tree / "nested/child/payload").read_bytes() == (source / "nested/child/payload").read_bytes(), "nested traversal differs")
    require(os.readlink(tree / "link") == "size-4096", "symlink differs")
    require((tree / "link").read_bytes() == files["size-4096"], "symlink resolution differs")


def mutation_and_sandbox(tree, repository, host_control):
    import errno
    result = {}
    for name, operation in {
        "write": lambda: (tree / "size-4096").write_bytes(b"bad"),
        "truncate": lambda: os.truncate(tree / "size-4096", 0),
        "chmod": lambda: (tree / "size-4096").chmod(0o777),
        "unlink": lambda: (tree / "size-4096").unlink(),
        "rename": lambda: (tree / "size-4096").rename(tree / "bad"),
        "mkdir": lambda: (tree / "bad").mkdir(),
        "symlink": lambda: (tree / "bad-link").symlink_to("size-4096"),
    }.items():
        try:
            operation()
        except OSError as error:
            require(error.errno in (errno.EROFS, errno.EACCES, errno.EPERM), f"unexpected mutation error: {name}: {error}")
            result[name] = error.errno
        else:
            raise common.BenchmarkError(f"mutation succeeded: {name}")
    policy = f'(version 1)(allow default)(deny file-read* (subpath {json.dumps(str(tree.resolve()))}) (subpath {json.dumps(str(repository.resolve()))}))'
    script = "import pathlib,sys; assert pathlib.Path(sys.argv[1]).read_bytes();\nfor p in sys.argv[2:]:\n try: pathlib.Path(p).read_bytes()\n except (PermissionError,FileNotFoundError): continue\n raise SystemExit(9)"
    child = subprocess.run(["/usr/bin/sandbox-exec", "-p", policy, os.sys.executable, "-c", script,
                            str(host_control), str(tree / "size-4096"), str(repository / "snapshot.json")], capture_output=True, timeout=30)
    require(child.returncode == 0, f"sandbox path/backing-store gate failed: {child.stderr!r}")
    result["sandbox_mount_and_repository_denied"] = True
    return result


def busy_unmount(mountpoint, file):
    with file.open("rb") as held:
        attempt = subprocess.run(["/sbin/umount", str(mountpoint)], capture_output=True, timeout=30)
        require(attempt.returncode != 0 and os.path.ismount(mountpoint), "busy mount was detached")
        require(bool(os.pread(held.fileno(), 1, 0)), "busy unmount broke existing reader")
    return {"returncode":attempt.returncode, "stderr":attempt.stderr.decode(errors="replace")}


def source_identity():
    paths = {Path(__file__), Path(memory.__file__), Path(__file__).with_name("native_fskit_launch.py"),
             Path(__file__).with_name("native_fskit_first_launch.py"),
             Path(__file__).with_name("native_fskit_workloads.py"),
             Path(__file__).with_name("native_fskit_launch_density.py"), cli.ROOT / "Cargo.toml"}
    for root in (cli.ROOT / "crates/casita/src", cli.ROOT / "crates/casita-fs/src", cli.ROOT / "crates/fskit-native", memory.SOURCE):
        paths.update(p for p in root.rglob("*") if p.is_file() and "target" not in p.parts
                     and (p.suffix in (".rs", ".toml", ".plist", ".entitlements", ".lock") or p.name == "Cargo.lock"))
    paths.add(cli.ROOT / "crates/casita-fs/Cargo.toml")
    paths.add(cli.ROOT / "crates/casita/Cargo.toml")
    return {str(p.relative_to(cli.ROOT)): hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(paths)}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="smoke")
    parser.add_argument("--repetitions", type=int, default=3)
    parser.add_argument("--timeout-seconds", type=int, default=1200,
                        help="deadline for all timed rounds; failure retains partial results and cleans mounts")
    parser.add_argument("--prepare-only", action="store_true")
    parser.add_argument("--launch-only", action="store_true")
    parser.add_argument("--directory-cache", choices=("enabled", "disabled"), default="enabled",
                        help="native immutable-directory cache control")
    parser.add_argument("--enumeration-attributes", choices=("requested", "eager"), default="requested")
    parser.add_argument("--enumeration-cache", choices=("enabled", "disabled"), default="enabled")
    parser.add_argument("--enumeration-timing", choices=("basic", "detailed"), default="basic")
    parser.add_argument("--filename-construction", choices=("data", "bytes"), default="data")
    parser.add_argument("--volume-capabilities", choices=("minimal", "explicit"), default="minimal")
    parser.add_argument("--item-timestamps", choices=("zero", "store"), default="store")
    parser.add_argument("--first-launch", action="store_true")
    parser.add_argument("--workloads", action="store_true")
    parser.add_argument("--workload-workers", type=int, nargs="+", choices=(1,8,15,16,17,31,32,33))
    parser.add_argument("--trace-read-ranges", action="store_true", help="bounded native read-range diagnostics; timings include tracing")
    parser.add_argument("--reader-cache-capacity", type=int, choices=(16,32), default=32)
    parser.add_argument("--reader-cache", choices=("enabled", "disabled"), default="enabled")
    parser.add_argument("--sample-extension-seconds", type=int, default=0,
                        help="rootless stack sample of this run's native extension; timings are diagnostic")
    parser.add_argument("--metadata-files", type=int, default=256)
    parser.add_argument("--xattr-mode", choices=("explicit", "emulated"), default="emulated")
    parser.add_argument("--bundle", type=Path)
    parser.add_argument("--server-binary", type=Path)
    args = parser.parse_args(argv)
    require(not args.workload_workers or (args.workloads and len(args.workload_workers)==len(set(args.workload_workers))), "workload worker subset requires workloads and unique counts")
    require(args.repetitions > 0, "positive repetitions required")
    require(args.timeout_seconds > 0, "positive timeout required")
    require(0 <= args.sample_extension_seconds <= 60, "extension sample duration must be 0..60 seconds")
    require(args.metadata_files >= 0 and (args.launch_only or args.metadata_files == 256),
            "nonnegative metadata count; custom counts require launch-only")
    require(platform.system() == "Darwin" and int(platform.mac_ver()[0].split('.')[0]) >= 26, "repository path resources require macOS 26+")
    require(os.getuid() != 0 and os.geteuid() != 0, "run without root")
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="casita-native-repository-"))
    result = dict(schema_version=2, result_schema="casita.native-fskit-repository.v2", complete=False,
                  comparison_complete=False, decision_eligible=False, rootless_mount=False,
                  work_directory=str(work), configuration={"repository": True, "profile":args.profile,
                  "repetitions":args.repetitions, "timeout_seconds":args.timeout_seconds, "launch_only":args.launch_only,
                  "native_directory_cache":args.directory_cache,
                  "native_enumeration_cache":args.enumeration_cache,
                  "native_enumeration_timing":args.enumeration_timing,
                  "native_filename_construction":args.filename_construction,
                  "native_volume_capabilities":args.volume_capabilities,
                  "native_item_timestamps":args.item_timestamps,
                  "first_launch":args.first_launch,
                  "workloads":args.workloads, "workload_workers":args.workload_workers,
                  "native_reader_cache":args.reader_cache,
                  "native_reader_cache_capacity":args.reader_cache_capacity,
                  "native_trace_read_ranges":args.trace_read_ranges,
                  "sample_extension_seconds":args.sample_extension_seconds,
                  "native_enumeration_attributes":args.enumeration_attributes, "metadata_files":args.metadata_files,
                  "native_xattr_mode":args.xattr_mode,
                  "cache_policy":"fresh-mount first-touch, correctness and lifecycle probes, rewarm, timed rounds",
                  "storage":"Repository::local snapshot with bounded native reader reuse; ordinary host control",
                  "portable_names":False}, environment={"uid":os.getuid(), "effective_uid":os.geteuid(), "platform":platform.platform()},
                  commands=[], samples=[], rounds=[], gates={}, comparisons=[], source_sha256=source_identity())
    run = RepositoryRun(work, result)
    previous_alarm = None
    sampler = None
    sample_log = None
    def save(): common.write_atomic(output, json.dumps(result, indent=2)+"\n")
    save()
    try:
        if args.bundle:
            bundle = args.bundle.resolve()
            require(json.loads((bundle / "Contents/Resources/source.json").read_text()) == result["source_sha256"], "bundle sources differ")
            result["build_identity"] = run.build_identity()
            require(json.loads((bundle / "Contents/Resources/build.json").read_text()) == result["build_identity"], "bundle build identity differs")
            run.command(["codesign", "--verify", "--deep", "--strict", bundle])
            run.register(bundle)
            result["bundle"] = str(bundle)
            native_binary = bundle / "Contents/Extensions/casita-native-fskit-extension.appex/Contents/MacOS/casita-native-fskit-extension"
            host_binary = bundle / "Contents/MacOS/casita-native-fskit"
            result["binaries"] = {binary.name:hashlib.sha256(binary.read_bytes()).hexdigest()
                                  for binary in (native_binary, host_binary)}
            require(args.server_binary is not None, "bundle reuse requires its matching --server-binary")
            server = args.server_binary.resolve()
            receipt = json.loads(server.with_suffix(".receipt.json").read_text())
            require(receipt["source_sha256"] == result["source_sha256"] and receipt["build_identity"] == result["build_identity"], "server and extension build receipts differ")
            require(receipt["sha256"] == hashlib.sha256(server.read_bytes()).hexdigest(), "server binary changed")
            require(receipt["exec_sha256"] == hashlib.sha256(server.with_name("casita-exec-fixture").read_bytes()).hexdigest(), "executable fixture changed")
        else:
            run.prepare("-")
            server = work / "casita-repository-fixture"
            shutil.copy2(Path(result["build_target_directory"]) / "release/casita-repository-fixture", server)
            shutil.copy2(Path(result["build_target_directory"]) / "release/casita-exec-fixture", server.with_name("casita-exec-fixture"))
            server.with_suffix(".receipt.json").write_text(json.dumps({"source_sha256":result["source_sha256"],
                "build_identity":result["build_identity"], "sha256":hashlib.sha256(server.read_bytes()).hexdigest(),
                "exec_sha256":hashlib.sha256(server.with_name("casita-exec-fixture").read_bytes()).hexdigest()}))
        result["server_binary"] = str(server)
        result["server_sha256"] = hashlib.sha256(server.read_bytes()).hexdigest()
        if args.prepare_only:
            result["status"] = "prepared and activated through Rust setup"
            return 0
        files = fixture(work / "source", "primary", server.with_name("casita-exec-fixture"), args.metadata_files)
        if args.workloads:
            from benchmarks.suites.native_fskit_workloads import add_fixture
            result["workload_fixture"] = add_fixture(work / "source", files)
        require(run.command([work / "source/native-executable"]) == b"native-ok\n", "host executable control failed")
        run.command([server, "import", work / "source", work / "repository"], timeout=600)
        result["snapshot"] = json.loads((work / "repository/snapshot.json").read_text())
        # All compilation/import is completed before either implementation is timed.
        fixture(work / "canary-source", "independent-canary", server.with_name("casita-exec-fixture"), args.metadata_files)
        run.command([server, "import", work / "canary-source", work / "canary-repository"], timeout=600)
        late = work / "late-source"
        late.mkdir(); (late / "published").write_bytes(b"publication-after-miss\n")
        run.command([server, "stage", late, work / "repository", "later"], timeout=600)
        if args.directory_cache == "disabled":
            for repository in (work / "repository", work / "canary-repository"):
                (repository / "disable-directory-cache").touch()
        if args.enumeration_attributes == "eager":
            for repository in (work / "repository", work / "canary-repository"):
                (repository / "eager-enumeration-attributes").touch()
        if args.enumeration_cache == "disabled":
            for repository in (work / "repository", work / "canary-repository"):
                (repository / "disable-enumeration-cache").touch()
        if args.enumeration_timing == "detailed":
            for repository in (work / "repository", work / "canary-repository"):
                (repository / "time-enumeration-phases").touch()
        if args.filename_construction == "bytes":
            for repository in (work / "repository", work / "canary-repository"):
                (repository / "filename-from-bytes").touch()
        if args.volume_capabilities == "explicit":
            for repository in (work / "repository", work / "canary-repository"):
                (repository / "explicit-volume-capabilities").touch()
        if args.item_timestamps == "zero":
            for repository in (work / "repository", work / "canary-repository"):
                (repository / "zero-timestamps").touch()
        if args.trace_read_ranges:
            for repository in (work / "repository", work / "canary-repository"):
                (repository / "trace-read-ranges").touch()
        for repository in (work / "repository", work / "canary-repository"):
            (repository / "reader-cache-capacity").write_text(str(args.reader_cache_capacity))
        if args.reader_cache == "disabled":
            for repository in (work / "repository", work / "canary-repository"):
                (repository / "disable-reader-cache").touch()
        if args.xattr_mode == "explicit":
            for repository in (work / "repository", work / "canary-repository"):
                (repository / "explicit-xattrs").touch()
        native = run.mount_repository(work / "repository", "native")
        trees = {NATIVE:native / "views/fixture"}
        stats_sequence = itertools.count()
        def stats(name):
            # Distinct inode per snapshot prevents the VFS caching instrumentation.
            return json.loads((native / f"__casita_stats-{next(stats_sequence)}").read_text()) if name == NATIVE else {}
        for name, tree in trees.items():
            before = stats(name)
            for size in memory.SIZES:
                case_before = stats(name)
                started = time.perf_counter_ns()
                require((tree / f"size-{size}").read_bytes() == files[f"size-{size}"], "first-touch differs")
                elapsed = time.perf_counter_ns()-started
                result["samples"].append(dict(implementation=name, case="fresh-mount-first-touch", size=size, repetition=0,
                    elapsed_ns=[elapsed], p50_ns=elapsed, p95_ns=elapsed, correctness="passed",
                    counters_before=case_before, counters_after=stats(name)))
            result["gates"][name+"-first-touch-counters"] = {"before":before, "after":stats(name)}
            require(result["gates"][name+"-first-touch-counters"]["after"]["blob_opens"] > before["blob_opens"], "repository counters did not observe first-touch reads")
        for name, tree in trees.items():
            correctness(tree, work / "source", files)
            for case in EXECUTION_CASES:
                elapsed = execute(tree, case)
                result["samples"].append(dict(implementation=name, case=case+"-first", repetition=0,
                    elapsed_ns=[elapsed], p50_ns=elapsed, p95_ns=elapsed, correctness="passed"))
            save()
            result["gates"][name] = mutation_and_sandbox(tree, work / "repository", work / "source/namespace")
            if args.launch_only:
                from benchmarks.suites import native_fskit_launch as launch
                launch.xattr_mutation_gate(tree)
                result["gates"][name]["xattr_mutation_denied"] = True
            parent = tree.parent
            require("later" not in os.listdir(parent) and not (parent / "later").exists(), "publication was already visible")
            (parent / "later").mkdir()
            require((parent / "later/published").read_bytes() == b"publication-after-miss\n", "cached negative publication failed")
            require("later" in os.listdir(parent), "cached listing hides publication")
            result["gates"][name]["publication_after_cached_miss"] = True
            result["gates"][name]["busy_unmount"] = busy_unmount(native, tree / "size-4096")
        native_pid = stats(NATIVE)["pid"]
        executable_path = run.command(["/bin/ps", "-p", str(native_pid), "-o", "comm="]).decode().strip()
        expected_executable = Path(result["bundle"]) / "Contents/Extensions/casita-native-fskit-extension.appex/Contents/MacOS/casita-native-fskit-extension"
        require(Path(executable_path).resolve() == expected_executable.resolve(), "FSKit launched a stale extension bundle")
        result["native_process"] = {"pid":native_pid, "executable":executable_path}
        listeners = subprocess.run(["/usr/sbin/lsof", "-nP", "-a", "-p", str(native_pid), "-iTCP", "-sTCP:LISTEN"], capture_output=True)
        require(listeners.returncode == 1 and not listeners.stdout, "native extension has a TCP listener or inspection failed")
        result["gates"]["native_no_tcp_listener"] = {"pid":native_pid, "status":"passed"}
        second_native = run.mount_repository(work / "canary-repository", "native-canary")
        require((second_native / "views/fixture/namespace").read_bytes() == b"independent-canary", "native namespace collision")
        require(all((tree / "namespace").read_bytes() == b"primary" for tree in trees.values()), "primary namespace changed")
        run.unmount(second_native)
        require(all((tree / "namespace").read_bytes() == b"primary" for tree in trees.values()), "second teardown broke first")
        result["gates"]["independent_repositories_and_teardown"] = "passed"
        # Even a refused unmount may purge VFS caches. Rewarm after lifecycle
        # probes so their side effects cannot bias the warm comparison.
        for name, tree in trees.items():
            correctness(tree, work / "source", files)
            for case in EXECUTION_CASES:
                execute(tree, case)
        result["gates"]["rewarmed_after_lifecycle"] = "passed"
        if args.launch_only:
            from benchmarks.suites.native_fskit_launch import volume_capabilities
            result["volume_capabilities"] = {name:volume_capabilities(tree) for name, tree in trees.items()}
            result["volume_capabilities"]["host"] = volume_capabilities(work / "source")
            native_caps = result["volume_capabilities"][NATIVE]
            if args.volume_capabilities == "explicit":
                expected = 0x00020720  # 64-bit IDs, fast statfs, no root times, case sensitive/preserving.
                require(native_caps["capabilities"][0] & native_caps["valid"][0] & expected == expected,
                        "explicit volume capabilities did not reach VFS")
            require(not (native / "views/fixture/NATIVE-EXECUTABLE").exists(), "case-sensitive lookup failed")
            result["item_timestamps"] = {}
            for name, tree in trees.items():
                attributes = (tree / "native-executable").stat()
                observed = {key:getattr(attributes, key) for key in ("st_atime_ns", "st_mtime_ns", "st_ctime_ns")}
                result["item_timestamps"][name] = observed
                expected = 0 if name == NATIVE and args.item_timestamps == "zero" else 1_000_000_000
                require(all(value == expected for value in observed.values()), f"{name} timestamp control differs: {observed}")
        if args.sample_extension_seconds:
            stack_path = output.with_suffix(".stacks.txt")
            sample_log = output.with_suffix(".sample.log").open("w")
            command = ["/usr/bin/sample", str(native_pid), str(args.sample_extension_seconds),
                       "1", "-file", str(stack_path)]
            sampler = subprocess.Popen(command, stdout=sample_log, stderr=subprocess.STDOUT)
            result["extension_sample"] = {"pid":native_pid, "command":command,
                                           "stacks":str(stack_path), "status":"running"}
            save()
        iterations = 3 if args.profile == "smoke" else 30
        # The nested directory participates in enumeration; only regular boundary files are read by this workload.
        listing_files = {**files, "nested":None}
        previous_alarm = signal.signal(signal.SIGALRM, deadline_expired)
        signal.alarm(args.timeout_seconds)
        trees["host"] = work / "source"
        for repetition in range(args.repetitions):
            names = [NATIVE,"host"] if repetition % 2 == 0 else ["host",NATIVE]
            processes = run.command(["/bin/ps", "-axo", "pcpu,comm"]).decode().splitlines()[1:]
            busy = sorted(processes, key=lambda line: float(line.split(maxsplit=1)[0]), reverse=True)[:12]
            result["rounds"].append({"repetition":repetition, "order":names, "load":os.getloadavg(), "host_processes":busy})
            if args.launch_only:
                from benchmarks.suites import native_fskit_launch as launch
                launch.run_round({**trees, "host":work / "source"},
                    lambda name: {} if name == "host" else stats(name), repetition,
                    3 if args.profile == "smoke" else 10, result, save)
                continue
            for name in names:
                before = stats(name)
                result["samples"].extend(memory.measurements(trees[name], listing_files, name, repetition, iterations))
                for case in EXECUTION_CASES:
                    latencies = [execute(trees[name], case) for _ in range(1 if args.profile == "smoke" else 3)]
                    result["samples"].append(dict(implementation=name, case=case, repetition=repetition,
                        elapsed_ns=latencies, p50_ns=sorted(latencies)[len(latencies)//2],
                        p95_ns=max(latencies), correctness="passed"))
                result["rounds"][-1][name] = {"before":before, "after":stats(name)}
                save()
        paired = [{**row, "implementation":memory.NATIVE if row["implementation"] == NATIVE else memory.HOST}
                  for row in result["samples"] if row["case"] != "fresh-mount-first-touch" and not row["case"].endswith("-first")]
        if args.launch_only:
            result["comparisons"] = launch.comparisons(result["samples"], args.repetitions, NATIVE)
        else:
            result["comparisons"] = memory.paired_comparisons(paired, args.repetitions, result["snapshot"]["digest"],
                [(case, None, None) for case in EXECUTION_CASES])
        if sampler is not None:
            status = sampler.wait(timeout=args.sample_extension_seconds + 15)
            result["extension_sample"]["returncode"] = status
            require(status == 0 and stack_path.is_file() and stack_path.stat().st_size > 0,
                    "extension stack sample failed")
            result["extension_sample"]["status"] = "passed"
        run.unmount(native)
        result["native_teardown"] = json.loads((work / "repository/native-final.json").read_text())
        require(result["native_teardown"]["repository_release_barrier"] == "passed", "native reader release failed")
        if args.first_launch:
            from benchmarks.suites.native_fskit_first_launch import run_trials
            run_trials(run, server, work, files, args.repetitions, result, save)
            result["reader_cache_pressure"] = []
            for width in (15, 16, 17):
                measured = json.loads(run.command([server, "reader-cache", work / "repository", str(width),
                                                   "3" if args.profile == "smoke" else "20"], timeout=120))
                require(measured["correctness"] == "passed" and measured["repository_release_barrier"] == "passed",
                        "reader-cache pressure gate failed")
                result["reader_cache_pressure"].append(measured)
                save()
        if args.workloads:
            from benchmarks.suites.native_fskit_workloads import run_trials
            run_trials(run, server, work, files, args.repetitions, result, save)
        result.update(complete=True, comparison_complete=True, status="real-repository comparison measured")
    except Exception as error:
        result.update(status="failed", error=str(error))
    finally:
        if sampler is not None and sampler.poll() is None:
            sampler.terminate()
            try: sampler.wait(timeout=5)
            except subprocess.TimeoutExpired:
                sampler.kill(); sampler.wait()
            result["extension_sample"]["status"] = "interrupted"
        if sample_log is not None:
            sample_log.close()
        if previous_alarm is not None:
            signal.alarm(0)
            signal.signal(signal.SIGALRM, previous_alarm)
        if not run.cleanup():
            result.update(complete=False, comparison_complete=False)
        save()
        print(f"native-fskit-repository: {result.get('status')}: {result.get('error','')}; {output}", flush=True)
    return 0 if result["complete"] else 1

if __name__ == "__main__":
    raise SystemExit(main())
