"""Raw S3 controls to separate hot-key CAS conflicts from HTTP 503 failures.

No SDK retries, no Casita, separate spawned clients. Production runs require an
explicit dedicated bucket/prefix and AWS credentials supplied through environment.
"""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import hmac
import json
import multiprocessing as mp
import os
import pathlib
import shutil
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
import xml.etree.ElementTree as ET

from benchmarks import cli
from benchmarks.suites import repository as common
from benchmarks.suites.pin_protocol import csv_int
from benchmarks.suites.pack.s3_gc import Rustfs
from benchmarks.suites.transfer.s3_path import create_rustfs_bucket, _sigv4_key


class Client:
    def __init__(self, config):
        self.config = config
        self.events = []

    def request(self, method, key, body=b"", condition=None, *, query=None, bucket_request=False):
        c = self.config
        parsed = urllib.parse.urlsplit(c["endpoint"])
        location = c["bucket"] + "/" if bucket_request else c["bucket"] + "/" + c["prefix"] + "/" + key
        path = "/" + urllib.parse.quote(location, safe="/~-._")
        query_string = urllib.parse.urlencode(sorted(query or []), quote_via=urllib.parse.quote)
        now = dt.datetime.now(dt.timezone.utc)
        date, timestamp = now.strftime("%Y%m%d"), now.strftime("%Y%m%dT%H%M%SZ")
        digest = hashlib.sha256(body).hexdigest()
        headers = {"host": parsed.netloc, "x-amz-content-sha256": digest, "x-amz-date": timestamp}
        if condition:
            headers[condition[0]] = condition[1]
        if c["local"]:
            access, secret, token = "minio", "minio123", None
        else:
            access, secret = os.environ["AWS_ACCESS_KEY_ID"], os.environ["AWS_SECRET_ACCESS_KEY"]
            token = os.environ.get("AWS_SESSION_TOKEN")
        if token:
            headers["x-amz-security-token"] = token
        signed = ";".join(sorted(headers))
        canonical = "".join(f"{k}:{headers[k]}\n" for k in sorted(headers))
        request = f"{method}\n{path}\n{query_string}\n{canonical}\n{signed}\n{digest}"
        scope = f"{date}/{c['region']}/s3/aws4_request"
        to_sign = f"AWS4-HMAC-SHA256\n{timestamp}\n{scope}\n{hashlib.sha256(request.encode()).hexdigest()}"
        signature = hmac.new(_sigv4_key(secret, date, c["region"], "s3"), to_sign.encode(), hashlib.sha256).hexdigest()
        headers["Authorization"] = f"AWS4-HMAC-SHA256 Credential={access}/{scope}, SignedHeaders={signed}, Signature={signature}"
        url = c["endpoint"].rstrip("/") + path + ("?" + query_string if query_string else "")
        req = urllib.request.Request(url, method=method,
                                     headers=headers, data=body if method == "PUT" else None)
        begin = time.monotonic()
        try:
            try:
                response = urllib.request.urlopen(req, timeout=20)
            except urllib.error.HTTPError as error:
                response = error
            with response:
                data = response.read()
                status = response.code
                etag = response.headers.get("ETag")
                request_id = response.headers.get("x-amz-request-id")
            code = None
            if status >= 400:
                try:
                    code = ET.fromstring(data).findtext("Code")
                except ET.ParseError:
                    code = "non-xml-error"
            self.events.append(dict(method=method, status=status, seconds=time.monotonic()-begin,
                                    code=code, request_id=request_id, key=key,
                                    error_body=data[:4096].decode(errors="replace") if status >= 400 else None))
            return status, data, etag
        except OSError as error:
            self.events.append(dict(method=method, status=0, seconds=time.monotonic()-begin,
                                    code=type(error).__name__, key=key))
            return 0, b"", None


def client_process(config, mode, index, operations, ready, start, result):
    client = Client(config)
    ready.put(index)
    if not start.wait(60):
        raise TimeoutError("client start")
    acknowledgments = []
    key = f"disjoint/{index}" if mode == "disjoint-put" else "hot"
    for operation in range(operations):
        receipt = f"{index}:{operation}"
        if mode == "hot-cas":
            status, data, etag = client.request("GET", key)
            if status != 200 or not etag:
                continue
            data = json.dumps([*json.loads(data), receipt]).encode()
            condition = ("if-match", etag)
        else:
            data, condition = b"raw-backend-control", None
        status, _, _ = client.request("PUT", key, data, condition)
        if status == 200:
            acknowledgments.append(receipt)
    result.put(dict(pid=os.getpid(), index=index, acknowledgments=acknowledgments, events=client.events))


def run_case(config, mode, writers, operations):
    ctx = mp.get_context("spawn")
    ready, result, start = ctx.Queue(), ctx.Queue(), ctx.Event()
    setup = Client(config)
    if mode != "disjoint-put":
        assert setup.request("PUT", "hot", b"[]" if mode == "hot-cas" else b"raw-backend-control")[0] == 200
    children = [ctx.Process(target=client_process, args=(config, mode, i, operations, ready, start, result))
                for i in range(writers)]
    try:
        for child in children:
            child.start()
        for _ in children:
            ready.get(timeout=60)
        begin = time.monotonic()
        start.set()
        rows = [result.get(timeout=operations * 45 + 60) for _ in children]
        seconds = time.monotonic() - begin
        for child in children:
            child.join(30)
            assert child.exitcode == 0
        assert len({row["pid"] for row in rows}) == writers
        # New client, raw uncached GETs after every writer has exited.
        reader = Client(config)
        if mode == "hot-cas":
            status, data, _ = reader.request("GET", "hot")
            assert status == 200
            receipts = json.loads(data)
            acknowledged = [receipt for row in rows for receipt in row["acknowledgments"]]
            assert len(receipts) == len(set(receipts))
            assert set(acknowledged) <= set(receipts), "acknowledged CAS overwritten"
        else:
            for row in rows:
                if row["acknowledgments"]:
                    key = f"disjoint/{row['index']}" if mode == "disjoint-put" else "hot"
                    status, data, _ = reader.request("GET", key)
                    assert status == 200 and data == b"raw-backend-control"
        events = [event for row in rows for event in row["events"]]
        statuses = {str(status): sum(e["status"] == status for e in events)
                    for status in sorted({e["status"] for e in events})}
        return dict(mode=mode, writers=writers, operations=operations, seconds=seconds,
                    statuses=statuses, clients=rows, audit=reader.events, fresh_readback=True)
    finally:
        for child in children:
            if child.pid:
                if child.is_alive():
                    child.kill()
                child.join(10)
        ready.close()
        result.close()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("smoke", "standard"), default="standard")
    parser.add_argument("--writers", type=csv_int, default=[1, 10, 32, 64, 100])
    parser.add_argument("--operations", type=int)
    parser.add_argument("--repetitions", type=int, default=1)
    parser.add_argument("--modes", default="disjoint-put,hot-put,hot-cas")
    parser.add_argument("--lock-timeout-seconds", type=int, default=5)
    parser.add_argument("--endpoint")
    parser.add_argument("--bucket")
    parser.add_argument("--prefix")
    parser.add_argument("--region", default="us-east-1")
    parser.add_argument("--output", required=True, type=pathlib.Path)
    args = parser.parse_args(argv)
    modes = args.modes.split(",")
    if not set(modes) <= {"disjoint-put", "hot-put", "hot-cas"} or args.lock_timeout_seconds < 1:
        parser.error("valid modes and positive lock timeout required")
    if any((args.endpoint, args.bucket, args.prefix)) and not all((args.endpoint, args.bucket, args.prefix)):
        parser.error("production endpoint, dedicated bucket and prefix must be supplied together")
    if args.endpoint and not all(os.environ.get(k) for k in ("AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY")):
        parser.error("supply AWS credentials via environment")
    if args.operations is not None and args.operations < 1 or args.repetitions < 1:
        parser.error("positive operations/repetitions required")
    operations = args.operations or (8 if args.profile == "smoke" else 64)
    work = args.output.with_suffix(".artifacts")
    work.mkdir(parents=True, exist_ok=False)
    server = None
    result = dict(complete=False, suite_id="state-and-publication", samples=[],
                  evidence="raw HTTP controls, not Casita publication throughput",
                  environment=common.environment_metadata(cli.ROOT))
    try:
        if not args.endpoint:
            variable = "RUSTFS_OBJECT_LOCK_ACQUIRE_TIMEOUT"
            previous = os.environ.get(variable)
            os.environ[variable] = str(args.lock_timeout_seconds)
            try:
                server = Rustfs(work / "rustfs")
            finally:
                if previous is None:
                    os.environ.pop(variable, None)
                else:
                    os.environ[variable] = previous
            endpoint, bucket = server.endpoint, "pin-http-" + uuid.uuid4().hex[:12]
            create_rustfs_bucket(endpoint, bucket)
            binary = pathlib.Path(shutil.which("rustfs"))
            result["backend"] = dict(path=str(binary), sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                                     version=subprocess.check_output([str(binary), "--version"], text=True).strip(),
                                     lock_timeout_seconds=args.lock_timeout_seconds)
        else:
            endpoint, bucket = args.endpoint, args.bucket
            result["backend"] = dict(endpoint=endpoint, production_provider_unverified=True)
        config = dict(endpoint=endpoint, bucket=bucket, prefix=(args.prefix or "controls") + "/" + uuid.uuid4().hex,
                      region=args.region, local=server is not None)
        result["configuration"] = config
        for repetition in range(args.repetitions):
            for writers in args.writers:
                for mode in modes:
                    case = {**config, "prefix": f"{config['prefix']}/{repetition}/{writers}/{mode}"}
                    row = run_case(case, mode, writers, operations)
                    row["repetition"] = repetition + 1
                    result["samples"].append(row)
                    common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
        result["complete"] = True
    except BaseException as error:
        result["error"] = repr(error)
        raise
    finally:
        if server:
            server.close()
        common.write_atomic(args.output, json.dumps(result, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
