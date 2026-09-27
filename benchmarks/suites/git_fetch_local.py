"""Local-storage control for the S3 Git fetch investigation."""
import sys

from benchmarks.suites.git_fetch_s3 import main as run


def main(argv=None):
    return run([*(sys.argv[1:] if argv is None else argv), "--backend", "local"])


if __name__ == "__main__":
    raise SystemExit(main())
