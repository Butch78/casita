"""Same-guest kernel EROFS, Casita FUSE and ext4 comparison; driven by erofs_transports."""
import base64
import ctypes
import gzip
import json
import os
from pathlib import Path
import subprocess
import sys
import time

from benchmarks.suites import filesystem_transports as fs


def mount_image(image, target):
    target.mkdir()
    libc = ctypes.CDLL(None, use_errno=True)
    # Direct file-backed mount: no loop device or FUSE fallback.
    if libc.mount(os.fsencode(image), os.fsencode(target), b"erofs", 1 | 2 | 4, None):
        raise OSError(ctypes.get_errno(), "kernel EROFS mount failed")
    entries = [line.split() for line in Path("/proc/self/mountinfo").read_text().splitlines()]
    matching = [row for row in entries if row[4] == str(target)]
    fs.require(len(matching) == 1 and matching[0][matching[0].index("-") + 1] == "erofs",
               "expected a kernel EROFS mount")
    return matching[0]


def unmount(target):
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.umount2(os.fsencode(target), 0):
        raise OSError(ctypes.get_errno(), "EROFS teardown failed")
    fs.require(not os.path.ismount(target), "EROFS mount survived teardown")


def build_image(source, destination, compression, mkfs):
    command = [mkfs, "--quiet", "--workers=1", "-T0", "-U00000000-0000-0000-0000-000000000001"]
    if compression == "lz4":
        command += ["-zlz4"]
    command += [str(destination), str(source)]
    start = time.perf_counter()
    subprocess.run(command, check=True, capture_output=True)
    build_seconds = time.perf_counter() - start
    fd = os.open(destination, os.O_RDONLY)
    start = time.perf_counter()
    try:
        os.fsync(fd)
    finally:
        os.close(fd)
    sync_seconds = time.perf_counter() - start
    subprocess.run([str(Path(mkfs).with_name("fsck.erofs")), str(destination)], check=True, capture_output=True)
    info = destination.stat()
    return {"command": command, "build_seconds": build_seconds, "sync_seconds": sync_seconds,
            "image_bytes": info.st_size, "allocated_bytes": info.st_blocks * 512,
            "sha256": fs.digest(destination.read_bytes()), "fsck": "passed"}


def make_fixture(path, profile, corpus):
    contents, bulk = fs.fixture(path, profile)
    if corpus == "compressible":
        for name, data in list(contents.items()):
            if not name.startswith("file-"):
                continue
            pattern = name.encode() + b": reproducible compiler input and cached package data\n"
            data = (pattern * (len(data) // len(pattern) + 1))[:len(data)]
            contents[name] = data
            (path / name).write_bytes(data)
    return contents, bulk


def run(config):
    work = Path("/cache/benchmark")
    work.mkdir()
    result = {"complete": False, "security_complete": False, "samples": [], "preparation": [],
              "security": [], "guest_environment": fs.common.environment_metadata(work),
              "guest_mounts": Path("/proc/mounts").read_text(), "configuration": config}
    active_mounts, servers = [], []
    try:
        for corpus in config["corpora"]:
            source = work / corpus
            contents, bulk = make_fixture(source, config["profile"], corpus)
            for repetition in range(config["repetitions"]):
                base = work / f"{corpus}-{repetition}"
                base.mkdir()
                # Include export from Casita, rather than quietly treating an
                # already-materialized tree as free EROFS preparation.
                producer = fs.Server(Path("/transport_server"), source, base / "producer", 1)
                servers.append(producer)
                checkout = producer.call("checkout", timeout=180)
                fs.require(checkout.get("event") == "checkout", "checkout failed")
                exported = Path(checkout["path"])
                fs.integrity(exported, contents)
                producer_stop = producer.close()
                servers.remove(producer)
                prep = {"corpus": corpus, "repetition": repetition, "repository": producer.ready,
                        "checkout": checkout, "producer_stop": producer_stop, "images": {}}
                # Alternate the order of image building independently of reads.
                for compression in (["none", "lz4"] if repetition % 2 == 0 else ["lz4", "none"]):
                    prep["images"][compression] = build_image(exported, base / f"{compression}.erofs", compression, config["mkfs"])
                result["preparation"].append(prep)
                implementations = ["host-ext4", "casita-fuse", "erofs-none", "erofs-lz4"]
                shift = repetition % len(implementations)
                order = implementations[shift:] + implementations[:shift]
                for implementation in order:
                    print(f"EROFS comparison: {corpus} rep {repetition} {implementation}", flush=True)
                    server = None
                    mount_info = None
                    if implementation == "host-ext4":
                        tree = source
                    elif implementation == "casita-fuse":
                        server = fs.Server(Path("/transport_server"), source, base / "fuse", 1)
                        servers.append(server)
                        tree = server.tree
                    else:
                        image = base / f"{implementation.removeprefix('erofs-')}.erofs"
                        tree = base / implementation
                        start = time.perf_counter()
                        mount_info = mount_image(image, tree)
                        active_mounts.append(tree)
                        prep["images"][implementation.removeprefix("erofs-")]["mount_seconds"] = time.perf_counter() - start
                    rows = fs.run_cases(tree, contents, bulk, config["profile"], implementation, repetition, server)
                    rows += fs.cached_read_controls(tree, contents, bulk, implementation, repetition, server)
                    fs.integrity(tree, contents)
                    gates = {} if implementation == "host-ext4" else fs.security_gates(tree, source)
                    if implementation.startswith("erofs-"):
                        # A second, disjoint image must coexist and unmount independently.
                        other_source = base / (implementation + "-canary-source")
                        other_source.mkdir()
                        (other_source / "canary").write_bytes(b"independent image\n")
                        other_image = base / (implementation + "-canary.erofs")
                        build_image(other_source, other_image, "none", config["mkfs"])
                        other = base / (implementation + "-canary")
                        mount_image(other_image, other)
                        active_mounts.append(other)
                        fs.require((other / "canary").read_bytes() == b"independent image\n" and
                                   not (other / "file-00000").exists() and not (tree / "canary").exists(), "image namespaces crossed")
                        unmount(other)
                        active_mounts.remove(other)
                        fs.require((tree / "file-00000").read_bytes() == contents["file-00000"], "second teardown affected first")
                        gates["independent_mounts"] = {"status": "passed"}
                        start = time.perf_counter()
                        unmount(tree)
                        active_mounts.remove(tree)
                        prep["images"][implementation.removeprefix("erofs-")]["unmount_seconds"] = time.perf_counter() - start
                        gates["teardown"] = {"status": "passed"}
                    if server:
                        prep["fuse"] = {"ready": server.ready, "stopped": server.close()}
                        servers.remove(server)
                        gates["teardown"] = {"status": "passed"}
                    for row in rows:
                        row["corpus"] = corpus
                    result["samples"].extend(rows)
                    if gates:
                        result["security"].append({"corpus": corpus, "repetition": repetition,
                            "implementation": implementation, "gates": gates, "mountinfo": mount_info})
        result["security_complete"] = all(g["status"] == "passed" for r in result["security"] for g in r["gates"].values())
        result["complete"] = True
    except BaseException as error:
        result["error"] = f"{type(error).__name__}: {error}"
        for row in result["samples"]:
            row["status"] = "invalid"
        raise
    finally:
        for server in servers:
            server.abort()
        for target in reversed(active_mounts):
            try:
                unmount(target)
            except OSError:
                pass
        encoded = base64.b64encode(gzip.compress(json.dumps(result).encode())).decode()
        print("CASITA_EROFS_RESULT=" + encoded, flush=True)


if __name__ == "__main__":
    run(json.loads(Path(sys.argv[1]).read_text()))
