"""Repeat the permanent native-static-code case on any existing macOS file.

No Casita dependency, mount changes, process launches or timing claims. Observe
the filesystem's enumeration counters or sample this process to attribute scans.
The measured, correctness-gated version lives in native_fskit_launch.py and is
registered through native-fskit-launch and native-fskit-launch-density.
"""
import argparse
import ctypes
import os
from pathlib import Path
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("executable", type=Path)
    parser.add_argument("--iterations", type=int, default=100)
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("requires macOS")
    if not 1 <= args.iterations <= 100000:
        parser.error("iterations must be 1..100000")
    if not args.executable.is_file():
        parser.error("executable must be an existing file")
    cf = ctypes.CDLL("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")
    security = ctypes.CDLL("/System/Library/Frameworks/Security.framework/Security")
    cf.CFURLCreateFromFileSystemRepresentation.argtypes = [ctypes.c_void_p, ctypes.c_char_p,
                                                         ctypes.c_long, ctypes.c_ubyte]
    cf.CFURLCreateFromFileSystemRepresentation.restype = ctypes.c_void_p
    cf.CFRelease.argtypes = [ctypes.c_void_p]
    cf.CFRelease.restype = None
    security.SecStaticCodeCreateWithPath.argtypes = [ctypes.c_void_p, ctypes.c_uint32,
                                                  ctypes.POINTER(ctypes.c_void_p)]
    security.SecStaticCodeCreateWithPath.restype = ctypes.c_int32
    path = os.fsencode(args.executable.absolute())
    url = cf.CFURLCreateFromFileSystemRepresentation(None, path, len(path), False)
    if not url:
        raise RuntimeError("CFURL creation failed")
    print(f"pid={os.getpid()} iterations={args.iterations}", flush=True)
    try:
        for _ in range(args.iterations):
            code = ctypes.c_void_p()
            try:
                status = security.SecStaticCodeCreateWithPath(url, 0, ctypes.byref(code))
                if status != 0 or not code.value:
                    raise RuntimeError(f"code-object creation failed: {status}")
            finally:
                if code.value:
                    cf.CFRelease(code)
    finally:
        cf.CFRelease(url)
    print("All code-object creations succeeded.")


if __name__ == "__main__":
    main()
