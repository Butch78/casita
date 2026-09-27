"""Adversarial schedules for the proposed barrier, separate from timings."""
import hashlib
import json
import pathlib
import tempfile
import unittest
from unittest import mock

from benchmarks.suites.pin_protocol import Ambiguous, Protocol, Store, audit_all_roots, modify, payload


class ProtocolSafety(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.store = Store(self.directory.name)
        self.store.cas("registry", None, dict(closed=False, members=[]))
        self.store.cas("ledger", None, dict(closed=False, pins={}))

    def test_delayed_owner_write_cannot_undo_gc_freeze(self):
        protocol = Protocol(self.store, "owned")
        protocol.register("writer")
        record, old = self.store.get("writers/writer")
        self.store.edit("registry", lambda v: modify(v, closed=True))
        frozen = self.store.edit("writers/writer", lambda v: modify(v, frozen=True))
        self.assertFalse(self.store.cas("writers/writer", old, modify(record, resources=["late"])))
        self.assertEqual(self.store.get("writers/writer")[0], frozen)
        self.store.edit("writers/writer", lambda v: modify(v, frozen=False))
        self.assertEqual(self.store.get("writers/writer")[0], record)
        self.assertFalse(self.store.cas("writers/writer", old, modify(record, resources=["late"])))

    def test_delayed_admission_cannot_cross_collection_barrier(self):
        registry, old = self.store.get("registry")
        self.store.cas("writers/late", None, dict(frozen=False, resources=[]))
        self.store.edit("registry", lambda v: modify(v, closed=True))
        self.assertFalse(self.store.cas("registry", old, modify(registry, members=["late"])))
        self.assertEqual(self.store.get("registry")[0]["members"], [])
        self.store.edit("registry", lambda v: modify(v, closed=False))
        self.assertEqual(self.store.get("registry")[0], registry)
        self.assertFalse(self.store.cas("registry", old, modify(registry, members=["late"])))

    def test_crash_protection_survives_repeated_collections(self):
        for variant in ("current-shape", "owned"):
            with self.subTest(variant=variant):
                protocol = Protocol(self.store, variant)
                protocol.register(variant)
                protocol.protect(variant, [variant])
                self.store.cas(f"objects/{variant}", None, "payload")
                for _ in range(3):
                    protocol.collect()
                    self.assertIsNotNone(self.store.get(f"objects/{variant}")[0])
                protocol.release(variant)
                protocol.collect()
                self.assertIsNone(self.store.get(f"objects/{variant}")[0])

    def test_lost_response_is_durable_and_duplicate_retry_is_idempotent(self):
        with self.assertRaises(Ambiguous):
            self.store.cas("objects/ambiguous", None, "bytes", ambiguous=True)
        self.assertEqual(self.store.get("objects/ambiguous")[0], "bytes")
        self.assertFalse(self.store.cas("objects/ambiguous", None, "replacement"))
        protocol = Protocol(self.store, "owned")
        protocol.register("writer")
        protocol.protect("writer", ["a", "b"], ambiguous=True)
        protocol.protect("writer", ["a", "b"])
        self.assertEqual(self.store.get("writers/writer")[0]["resources"], ["a", "b"])

    def test_failure_audit_finds_outputs_without_ack_queue_messages(self):
        raw = payload("owner-0000", 0)
        key = hashlib.sha256(raw).hexdigest()
        self.store.cas(f"objects/{key}", None, raw.hex())
        self.store.cas("roots/owner-0000", None, [key])
        audit_all_roots(self.directory.name, 1, 1, "none")
        self.store.path(f"objects/{key}").unlink()
        with self.assertRaises(AssertionError):
            audit_all_roots(self.directory.name, 1, 1, "none")


class RemoteReporting(unittest.TestCase):
    def test_failure_does_not_hide_later_variants_or_pass_correctness(self):
        from benchmarks.suites import pin_protocol_s3 as remote
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            binary = root / "rustfs"
            binary.write_bytes(b"test artifact")
            server = mock.Mock(endpoint="http://127.0.0.1:1")
            original_store = remote.protocol.Store
            with (mock.patch.object(remote, "Rustfs", return_value=server),
                  mock.patch.object(remote, "create_rustfs_bucket"),
                  mock.patch.object(remote.shutil, "which", return_value=str(binary)),
                  mock.patch.object(remote.subprocess, "check_output", return_value="test-rustfs"),
                  mock.patch.object(remote.common, "environment_metadata", return_value={}),
                  mock.patch.object(remote.protocol, "sample", side_effect=[
                      RuntimeError("indeterminate PUT"),
                      {"status": "ok", "fresh_readback": True},
                      {"status": "ok", "fresh_readback": True},
                  ])):
                status = remote.main(["--writers", "1", "--objects", "9", "--faults", "none",
                                      "--output", str(root / "result.json")])
            result = json.loads((root / "result.json").read_text())
            self.assertEqual(status, 1)
            self.assertTrue(result["attempted_all"])
            self.assertFalse(result["complete"])
            self.assertEqual(len(result["samples"]), 3)
            self.assertFalse(result["samples"][0]["fresh_readback"])
            self.assertIs(remote.protocol.Store, original_store)
            server.close.assert_called_once()


if __name__ == "__main__":
    unittest.main()
