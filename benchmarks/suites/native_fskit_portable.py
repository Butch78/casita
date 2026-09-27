"""Paired native FSKit/host timings with identical portable filenames."""
import sys
from benchmarks.suites.native_fskit import main as native_main


def main(argv=None):
    return native_main(["--portable-names", *(sys.argv[1:] if argv is None else argv)])

if __name__ == "__main__":
    raise SystemExit(main())
