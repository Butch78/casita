"""Run the pin protocol prototypes on real RustFS conditional objects.

Reuses the process/fault/readback gates of pin_protocol. This is a protocol
prototype with JSON records, not Casita's production WAL/catalog implementation.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import multiprocessing as mp
import pathlib
import shutil
import subprocess
import sys
import uuid
import xml.etree.ElementTree as ET

from benchmarks import cli
from benchmarks.suites import pin_protocol as protocol
from benchmarks.suites import repository as common
from benchmarks.suites.pin_http import Client
from benchmarks.suites.pack.s3_gc import Rustfs
from benchmarks.suites.transfer.s3_path import create_rustfs_bucket

FileStore = protocol.Store
CONFIG = None
ACTIVE_PREFIX = None


class RemotePath:
    def __init__(self, store, key):
        self.store, self.key = store, key
        self.name = pathlib.PurePosixPath(key).name
        self.suffix = pathlib.PurePosixPath(key).suffix

    def glob(self, pattern):
        assert pattern == "*"
        prefix = self.store.client.config["prefix"] + "/"
        continuation = None
        while True:
            query = [("list-type", "2"), ("prefix", prefix + self.key + "/")]
            if continuation:
                query.append(("continuation-token", continuation))
            self.store.metrics["list"] += 1
            status, data, _ = self.store.client.request("GET", "", query=query, bucket_request=True)
            self.store.check(status, {200})
            document = ET.fromstring(data)
            for element in document.findall("{*}Contents/{*}Key"):
                key = element.text
                assert key.startswith(prefix + self.key + "/")
                yield RemotePath(self.store, key.removeprefix(prefix))
            if document.findtext("{*}IsTruncated") != "true":
                return
            continuation = document.findtext("{*}NextContinuationToken")
            assert continuation, "truncated S3 listing without continuation token"

    def unlink(self):
        self.store.metrics["delete"] += 1
        status, _, _ = self.store.client.request("DELETE", self.key)
        self.store.check(status, {200, 204})


class RemoteStore(FileStore):
    def __init__(self, root):
        super().__init__(root)
        config_path = self.root / "remote.json"
        if not config_path.exists():
            assert CONFIG is not None, "parent must initialize the fixture"
            config_path.write_text(json.dumps({**CONFIG, "prefix": CONFIG["prefix"] + "/" + uuid.uuid4().hex}))
            global ACTIVE_PREFIX
            ACTIVE_PREFIX = json.loads(config_path.read_text())["prefix"]
        self.client = Client(json.loads(config_path.read_text()))
        self.metrics.update(list=0, delete=0, http_503=0, http_409=0, http_412=0)

    def path(self, key):
        return RemotePath(self, key)

    def check(self, status, accepted):
        metric = f"http_{status}"
        if metric in self.metrics:
            self.metrics[metric] += 1
        if status not in accepted:
            raise RuntimeError(f"unresolved backend outcome: {self.client.events[-1]}")

    def get(self, key):
        self.metrics["get"] += 1
        self.metrics["pin_get"] += int(self.is_pin(key))
        status, raw, etag = self.client.request("GET", key)
        self.check(status, {200, 404})
        if status == 404:
            return None, None
        assert etag, "linearizable conditional reads require an ETag"
        self.metrics["read_bytes"] += len(raw)
        self.metrics["pin_read_bytes"] += len(raw) * self.is_pin(key)
        record = json.loads(raw)
        return record["value"], (etag, record["version"])

    def cas(self, key, version, value, ambiguous=False):
        self.metrics["put"] += 1
        self.metrics["pin_put"] += int(self.is_pin(key))
        sequence = version[1] if version else 0
        raw = json.dumps(dict(version=sequence + 1, value=value),
                         sort_keys=True, separators=(",", ":")).encode()
        self.metrics["write_bytes"] += len(raw)
        self.metrics["pin_write_bytes"] += len(raw) * self.is_pin(key)
        condition = ("if-match", version[0]) if version else ("if-none-match", "*")
        status, _, _ = self.client.request("PUT", key, raw, condition)
        self.check(status, {200, 409, 412})
        if status == 409 and self.client.events[-1]["code"] != "ConditionalRequestConflict":
            raise RuntimeError(f"unclassified conditional-write conflict: {self.client.events[-1]}")
        if status in (409, 412):
            self.metrics["conflicts"] += 1
            return False
        if ambiguous:
            self.metrics["ambiguous"] += 1
            raise protocol.Ambiguous("real conditional PUT succeeded; response hidden from protocol")
        return True


def remote_entry(name, *args):
    protocol.Store = RemoteStore
    assert name in {"worker", "collector", "audit", "audit_all_roots"}
    getattr(protocol, name)(*args)


class RemoteContext:
    def __init__(self):
        self.context = mp.get_context("spawn")

    def Queue(self):
        return self.context.Queue()

    def Event(self):
        return self.context.Event()

    def Process(self, *, target, args):
        return self.context.Process(target=remote_entry, args=(target.__name__, *args))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--writers", type=protocol.csv_int, default=[1, 10, 32, 64, 100])
    parser.add_argument("--objects", type=protocol.csv_int)
    parser.add_argument("--batch", type=int, default=8)
    parser.add_argument("--faults", default="none,crash,ambiguous,cancel")
    parser.add_argument("--repetitions", type=int, default=1)
    parser.add_argument("--fail-fast", action="store_true", help="stop after the first failed sample")
    parser.add_argument("--output", required=True, type=pathlib.Path)
    args = parser.parse_args(argv)
    counts = args.objects or ([9] if args.profile == "smoke" else [7, 8, 9, 64])
    faults = args.faults.split(",")
    if args.batch < 1 or args.repetitions < 1 or not set(faults) <= {"none", "crash", "ambiguous", "cancel"}:
        parser.error("positive batch/repetitions and valid faults required")
    work = args.output.with_suffix(".artifacts")
    work.mkdir(parents=True, exist_ok=False)
    result = dict(complete=False, suite_id="state-and-publication", samples=[],
                  evidence="protocol prototypes on real RustFS HTTP; JSON records, not Casita WAL/catalog publication",
                  environment=common.environment_metadata(cli.ROOT),
                  configuration=dict(writers=args.writers, objects=counts, batch=args.batch,
                                     faults=faults, repetitions=args.repetitions),
                  source_sha256={str(p): hashlib.sha256(p.read_bytes()).hexdigest()
                                 for p in (pathlib.Path(__file__), pathlib.Path(protocol.__file__),
                                           pathlib.Path(__file__).with_name("pin_http.py"))})
    server = None
    global CONFIG, ACTIVE_PREFIX
    original_store = protocol.Store
    try:
        server = Rustfs(work / "rustfs")
        bucket = "pin-protocol-" + uuid.uuid4().hex[:12]
        create_rustfs_bucket(server.endpoint, bucket)
        CONFIG = dict(endpoint=server.endpoint, bucket=bucket, prefix="protocol", region="us-east-1", local=True)
        binary = pathlib.Path(shutil.which("rustfs"))
        result["backend"] = dict(path=str(binary), sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                                 version=subprocess.check_output([str(binary), "--version"], text=True).strip(),
                                 endpoint=server.endpoint, bucket=bucket)
        protocol.Store = RemoteStore
        context = RemoteContext()
        for repetition in range(args.repetitions):
            for writers in args.writers:
                for count in counts:
                    for fault in faults:
                        variants = ["current-shape", "batched", "owned"]
                        if repetition % 2:
                            variants.reverse()
                        for variant in variants:
                            result["in_progress"] = dict(writers=writers, objects=count, fault=fault, variant=variant)
                            common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
                            ACTIVE_PREFIX = None
                            try:
                                row = protocol.sample(context, variant, writers, count, args.batch, fault)
                            except Exception as error:
                                if args.fail_fast:
                                    raise
                                row = dict(**result["in_progress"], status="failed", error=repr(error),
                                           fresh_readback=False)
                                row.update(getattr(error, "diagnostics", {}))
                                print(f"Failed protocol case: {row}", file=sys.stderr, flush=True)
                            row["prefix"] = ACTIVE_PREFIX
                            row["repetition"] = repetition + 1
                            result["samples"].append(row)
                            common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
        result.pop("in_progress", None)
        result["attempted_all"] = True
        result["complete"] = all(row["status"] == "ok" for row in result["samples"])
    except BaseException as error:
        result["error"] = repr(error)
        raise
    finally:
        protocol.Store = original_store
        CONFIG = None
        if server:
            server.close()
        common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
    return 0 if result["complete"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
