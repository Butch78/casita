"""Rootless launch latency: same repository, host controls, spawn/wait phases and I/O counters."""
import subprocess
import sys
import time
import fcntl
import os
import ctypes
import errno
import struct
from functools import lru_cache

from benchmarks.suites.filesystem_transports import require

CASES = ("script-timeout", "script-blocking", "script-interpreted-blocking",
         "native-timeout", "native-blocking", "native-posix-spawn-blocking",
         "native-fgetpath", "native-open-close", "native-getxattr", "native-listxattr",
         "native-realpath", "native-getattrname", "native-getattrpath", "native-static-code",
         "native-listdir", "native-bundle-discovery")


@lru_cache(maxsize=1)
def security_functions():
    require(sys.platform == "darwin", "Security framework control requires macOS")
    cf = ctypes.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
    security = ctypes.CDLL("/System/Library/Frameworks/Security.framework/Security")
    cf.CFURLCreateFromFileSystemRepresentation.argtypes = [ctypes.c_void_p, ctypes.c_char_p,
                                                         ctypes.c_long, ctypes.c_ubyte]
    cf.CFURLCreateFromFileSystemRepresentation.restype = ctypes.c_void_p
    cf.CFRelease.argtypes = [ctypes.c_void_p]
    cf.CFRelease.restype = None
    cf.CFBundleCreate.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
    cf.CFBundleCreate.restype = ctypes.c_void_p
    cf.CFBundleGetInfoDictionary.argtypes = [ctypes.c_void_p]
    cf.CFBundleGetInfoDictionary.restype = ctypes.c_void_p
    cf.CFBundleCopyExecutableURL.argtypes = [ctypes.c_void_p]
    cf.CFBundleCopyExecutableURL.restype = ctypes.c_void_p
    security.SecStaticCodeCreateWithPath.argtypes = [ctypes.c_void_p, ctypes.c_uint32,
                                                  ctypes.POINTER(ctypes.c_void_p)]
    security.SecStaticCodeCreateWithPath.restype = ctypes.c_int32
    return cf, security


def bundle_discovery_query(tree):
    """Exercise public bundle discovery without Security or process launch."""
    cf, _security = security_functions()
    path = os.fsencode(tree)
    url = cf.CFURLCreateFromFileSystemRepresentation(None, path, len(path), True)
    require(bool(url), "bundle directory URL creation failed")
    bundle = executable = None
    try:
        started = time.perf_counter_ns()
        bundle = cf.CFBundleCreate(None, url)
        require(bool(bundle), "plain fixture directory CFBundleCreate failed")
        cf.CFBundleGetInfoDictionary(bundle)  # Borrowed, owned by the bundle.
        executable = cf.CFBundleCopyExecutableURL(bundle)
        elapsed = time.perf_counter_ns() - started
        require(not executable, "plain fixture unexpectedly identified as executable bundle")
    finally:
        if executable:
            cf.CFRelease(executable)
        if bundle:
            cf.CFRelease(bundle)
        cf.CFRelease(url)
    return {"total_ns":elapsed, "spawn_ns":0, "communicate_ns":0,
            "bundle_discovery_ns":elapsed, "used_posix_spawn":False}


def static_code_query(tree, filename="native-executable"):
    cf, security = security_functions()
    path = os.fsencode(tree / filename)
    url = cf.CFURLCreateFromFileSystemRepresentation(None, path, len(path), False)
    require(bool(url), "CFURL creation failed")
    code = ctypes.c_void_p()
    try:
        started = time.perf_counter_ns()
        status = security.SecStaticCodeCreateWithPath(url, 0, ctypes.byref(code))
        elapsed = time.perf_counter_ns() - started
        require(status == 0 and bool(code.value), f"static-code creation failed: {status}")
    finally:
        if code.value:
            cf.CFRelease(code)
        cf.CFRelease(url)
    return {"total_ns":elapsed, "spawn_ns":0, "communicate_ns":0,
            "static_code_ns":elapsed, "used_posix_spawn":False}


class AttrList(ctypes.Structure):
    _fields_ = [("bitmapcount", ctypes.c_uint16), ("reserved", ctypes.c_uint16),
                ("commonattr", ctypes.c_uint32), ("volattr", ctypes.c_uint32),
                ("dirattr", ctypes.c_uint32), ("fileattr", ctypes.c_uint32),
                ("forkattr", ctypes.c_uint32)]


@lru_cache(maxsize=1)
def path_functions():
    require(sys.platform == "darwin", "Darwin path controls require macOS")
    library = ctypes.CDLL(None, use_errno=True)
    library.realpath.argtypes = [ctypes.c_char_p, ctypes.c_void_p]
    library.realpath.restype = ctypes.c_void_p
    library.getattrlist.argtypes = [ctypes.c_char_p, ctypes.POINTER(AttrList),
                                   ctypes.c_void_p, ctypes.c_size_t, ctypes.c_ulong]
    library.getattrlist.restype = ctypes.c_int
    return library


def attribute_string(buffer):
    raw = buffer.raw
    total, offset, length = struct.unpack_from("=IiI", raw)
    start = 4 + offset  # attrreference offsets are relative to the reference.
    require(12 <= total <= len(raw) and 12 <= start and length > 0
            and start + length <= total, "invalid getattrlist string bounds")
    value = raw[start:start+length]
    require(value[-1:] == b"\0" and b"\0" not in value[:-1], "invalid getattrlist string")
    return value[:-1]


def volume_capabilities(tree):
    library = path_functions()
    request = AttrList(5, 0, 0, 0x80020000, 0, 0, 0)
    buffer = ctypes.create_string_buffer(1024)
    returned = library.getattrlist(os.fsencode(tree), ctypes.byref(request), buffer, len(buffer), 0)
    require(returned == 0, f"volume capabilities query failed: errno {ctypes.get_errno()}")
    values = struct.unpack_from("=9I", buffer.raw)
    require(values[0] == 36, f"unexpected volume capabilities length: {values[0]}")
    return {"capabilities":list(values[1:5]), "valid":list(values[5:9])}


def path_query(tree, case):
    library = path_functions()
    path = os.fsencode(tree / "native-executable")
    expected = os.fsencode((tree / "native-executable").resolve())
    buffer = ctypes.create_string_buffer(4096)
    request = AttrList(5, 0, 1 if case == "native-getattrname" else 0x08000000, 0, 0, 0, 0)
    ctypes.set_errno(0)
    started = time.perf_counter_ns()
    if case == "native-realpath":
        returned = library.realpath(path, buffer)
    else:
        returned = library.getattrlist(path, ctypes.byref(request), buffer, len(buffer), 1)
    elapsed = time.perf_counter_ns() - started
    error = ctypes.get_errno()
    if case == "native-realpath":
        require(bool(returned), f"realpath failed: errno {error}")
        value = buffer.value
    else:
        require(returned == 0, f"getattrlist failed: errno {error}")
        value = attribute_string(buffer)
    require(value == (b"native-executable" if case == "native-getattrname" else expected),
            f"{case} returned wrong name/path: {value!r}")
    return {"total_ns":elapsed, "spawn_ns":0, "communicate_ns":0,
            "path_query_ns":elapsed, "used_posix_spawn":False}


@lru_cache(maxsize=1)
def xattr_functions():
    require(sys.platform == "darwin", "Darwin xattr controls require macOS")
    library = ctypes.CDLL(None, use_errno=True)
    library.getxattr.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_void_p, ctypes.c_size_t, ctypes.c_uint32, ctypes.c_int]
    library.getxattr.restype = ctypes.c_ssize_t
    library.listxattr.argtypes = [ctypes.c_char_p, ctypes.c_void_p, ctypes.c_size_t, ctypes.c_int]
    library.listxattr.restype = ctypes.c_ssize_t
    library.setxattr.argtypes = library.getxattr.argtypes
    library.setxattr.restype = ctypes.c_int
    library.removexattr.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_int]
    library.removexattr.restype = ctypes.c_int
    return library


def xattr_mutation_gate(tree):
    library = xattr_functions()
    path, name = os.fsencode(tree / "native-executable"), b"user.casita-benchmark"
    for operation in (lambda: library.setxattr(path, name, b"x", 1, 0, 0),
                      lambda: library.removexattr(path, name, 0)):
        ctypes.set_errno(0)
        returned = operation()
        error = ctypes.get_errno()
        require(returned == -1 and error in (errno.EROFS, errno.EACCES, errno.EPERM, errno.ENOTSUP, 93),
                f"xattr mutation was not denied: result={returned} errno={error}")


def xattr_query(tree, case):
    library = xattr_functions()
    path = os.fsencode(tree / "native-executable")
    ctypes.set_errno(0)
    started = time.perf_counter_ns()
    if case == "native-getxattr":
        returned = library.getxattr(path, b"com.apple.cs.CodeDirectory", None, 0, 0, 0)
    else:
        returned = library.listxattr(path, None, 0, 0)
    elapsed = time.perf_counter_ns()-started
    error = ctypes.get_errno()
    if case == "native-getxattr":
        require(returned == -1 and error in (93, errno.ENOTSUP), f"unexpected getxattr: {returned}, errno {error}")
    else:
        require(returned == 0 or (returned == -1 and error == errno.ENOTSUP), f"unexpected listxattr: {returned}, errno {error}")
    return {"total_ns":elapsed, "spawn_ns":0, "communicate_ns":0, "xattr_ns":elapsed,
            "used_posix_spawn":False, "returncode":returned, "errno":error if returned < 0 else 0}


class ObservedPopen(subprocess.Popen):
    used_posix_spawn = False

    def _posix_spawn(self, *args, **kwargs):
        self.used_posix_spawn = True
        return super()._posix_spawn(*args, **kwargs)


def launch(tree, case, expected_names=None):
    if case == "native-listdir":
        require(expected_names is not None, "listdir requires an independent names oracle")
        started = time.perf_counter_ns()
        names = os.listdir(tree)
        elapsed = time.perf_counter_ns() - started
        require(len(names) == len(expected_names) and set(names) == set(expected_names),
                "timed directory listing differs or repeats names")
        return {"total_ns":elapsed, "spawn_ns":0, "communicate_ns":0,
                "entries":len(names), "used_posix_spawn":False}
    if case == "native-bundle-discovery":
        return bundle_discovery_query(tree)
    if case == "native-static-code":
        return static_code_query(tree)
    if case in ("native-realpath", "native-getattrname", "native-getattrpath"):
        return path_query(tree, case)
    if case in ("native-getxattr", "native-listxattr"):
        return xattr_query(tree, case)
    if case in ("native-fgetpath", "native-open-close"):
        expected = str((tree / "native-executable").resolve()).encode()
        started = time.perf_counter_ns()
        path_ns = 0
        with (tree / "native-executable").open("rb") as handle:
            opened = time.perf_counter_ns()
            if case == "native-fgetpath":
                # Darwin sys/fcntl.h: F_GETPATH=50, buffer is MAXPATHLEN bytes.
                path = fcntl.fcntl(handle.fileno(), 50, bytes(1024)).split(b"\0", 1)[0]
                path_ns = time.perf_counter_ns() - opened
                require(path == expected, f"F_GETPATH returned wrong path: {path!r} != {expected!r}")
        return {"total_ns":time.perf_counter_ns()-started, "spawn_ns":0,
                "communicate_ns":0, "getpath_ns":path_ns, "used_posix_spawn":False}
    script = case.startswith("script")
    argv = [str(tree / ("run" if script else "native-executable"))]
    if "interpreted" in case:
        argv.insert(0, "/bin/sh")
    expected = b"casita-native-fskit-ok\n" if script else b"native-ok\n"
    started = time.perf_counter_ns()
    process = ObservedPopen(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            close_fds="posix-spawn" not in case)
    spawned = time.perf_counter_ns()
    try:
        stdout, stderr = process.communicate(timeout=120 if case.endswith("timeout") else None)
    except BaseException:
        process.kill()
        process.communicate()
        raise
    finished = time.perf_counter_ns()
    require(process.returncode == 0 and stdout == expected,
            f"{case}: exit={process.returncode}, stdout={stdout!r}, stderr={stderr!r}")
    if "posix-spawn" in case:
        require(process.used_posix_spawn, "requested posix_spawn control did not use posix_spawn")
    return {"total_ns":finished-started, "spawn_ns":spawned-started,
            "communicate_ns":finished-spawned, "used_posix_spawn":process.used_posix_spawn}


def run_round(trees, stats, repetition, iterations, result, save):
    expected_names = os.listdir(trees["host"])
    names = list(trees)
    names = names[repetition % len(names):] + names[:repetition % len(names)]
    result["rounds"][-1]["launch_order"] = names
    # Rotate case order too, so timeout/spawn controls do not always get the coldest path.
    cases = CASES[repetition % len(CASES):] + CASES[:repetition % len(CASES)]
    for case in cases:
        for name in names:
            observations = []
            for _ in range(iterations):
                before = stats(name)
                measured = launch(trees[name], case, expected_names=expected_names)
                measured.update(counters_before=before, counters_after=stats(name))
                observations.append(measured)
            values = sorted(row["total_ns"] for row in observations)
            result["samples"].append(dict(implementation=name, case=case, repetition=repetition,
                elapsed_ns=[row["total_ns"] for row in observations], phases=observations,
                p50_ns=values[len(values)//2], p95_ns=values[min(len(values)-1,int(.95*len(values)))],
                correctness="passed"))
            save()


def comparisons(samples, repetitions, native):
    selected = [row for row in samples if row["case"] in CASES]
    expected = {(r,c,n) for r in range(repetitions) for c in CASES for n in (native,"host")}
    keys = [(row["repetition"],row["case"],row["implementation"]) for row in selected]
    require(set(keys) == expected and len(keys) == len(expected), "missing or duplicate launch control")
    indexed = dict(zip(keys,selected))
    result = []
    for r in range(repetitions):
        for c in CASES:
            rows = [indexed[r,c,n] for n in (native,"host")]
            require(all(row["correctness"] == "passed" for row in rows), "failed launch sample")
            require(len({len(row["elapsed_ns"]) for row in rows}) == 1, "unmatched launch sample counts")
            result.append(dict(repetition=r,case=c,p50_native_over_host=rows[0]["p50_ns"]/rows[1]["p50_ns"]))
    return result


def main(argv=None):
    from benchmarks.suites.native_fskit_repository import main as repository_main
    return repository_main(["--launch-only", *(sys.argv[1:] if argv is None else argv)])


if __name__ == "__main__":
    raise SystemExit(main())
