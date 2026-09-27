"""Run catalog probes with the sharded reader phase against real RustFS objects."""
import json
import os
import pathlib
import tempfile
from benchmarks.suites.pack import catalog
from benchmarks.suites.pack.s3_gc import Rustfs
from benchmarks.suites.transfer.s3_path import create_rustfs_bucket
from benchmarks.suites import repository as common


def main(argv=None):
    args = catalog.build_parser().parse_args(argv)
    if args.output is None:
        raise common.BenchmarkError("s3-catalog-index requires --output")
    with tempfile.TemporaryDirectory(prefix="casita-sharded-s3-") as temporary:
        server = Rustfs(pathlib.Path(temporary) / "rustfs")
        overrides = {"CASITA_CATALOG_BENCH_S3_BUCKET": "casita-sharded", "AWS_ACCESS_KEY_ID": "minio",
            "AWS_SECRET_ACCESS_KEY": "minio123", "AWS_REGION": "us-east-1", "AWS_ENDPOINT": server.endpoint,
            "AWS_ENDPOINT_URL": server.endpoint, "AWS_ENDPOINT_URL_S3": server.endpoint, "AWS_ALLOW_HTTP": "true"}
        previous = {key: os.environ.get(key) for key in (*overrides, "AWS_SESSION_TOKEN", "AWS_PROFILE")}
        try:
            create_rustfs_bucket(server.endpoint, "casita-sharded")
            os.environ.update(overrides)
            os.environ.pop("AWS_SESSION_TOKEN", None)
            os.environ.pop("AWS_PROFILE", None)
            status = catalog.main(argv)
            result = json.loads(args.output.read_text())
            result["configuration"]["sharded_operations_backend"] = "RustFS S3; other codec and rebase phases use local files"
            common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
            report = args.report or args.output.with_suffix(".md")
            report.write_text(report.read_text() + "\nSharded open, point lookup, enumeration and metadata GC used real RustFS S3 objects. Fixture upload was outside timing; codec and streaming-rebase probes used local files.\n")
            return status
        finally:
            for key, value in previous.items():
                if value is None:
                    os.environ.pop(key, None)
                else:
                    os.environ[key] = value
            server.close()

if __name__ == "__main__":
    raise SystemExit(main())
