import json
import pathlib
import tempfile
import unittest
from unittest import mock

from benchmarks import all as all_suites, cli
from benchmarks.suites import filesystem_transports as suite


class FilesystemTransportTests(unittest.TestCase):
    def test_fixture_has_size_boundaries_and_byte_names(self):
        with tempfile.TemporaryDirectory() as work:
            root = pathlib.Path(work) / "source"
            contents, bulk = suite.fixture(root, "smoke")
            self.assertTrue(set(suite.BOUNDARIES) <= {len(data) for data in contents.values()})
            self.assertEqual(len(contents[bulk]), 8 * 1024**2)
            self.assertIn(b"byte-ascii" if suite.platform.system() == "Darwin" else b"byte-\xff",
                          {suite.os.fsencode(name) for name in contents})
            suite.integrity(root, contents)

    def test_correctness_gate_rejects_corrupted_content(self):
        with tempfile.TemporaryDirectory() as work:
            root = pathlib.Path(work) / "source"
            contents, _ = suite.fixture(root, "smoke")
            path = root / "file-00000"
            path.chmod(0o600)
            path.write_bytes(b"wrong")
            with self.assertRaisesRegex(suite.common.BenchmarkError, "content mismatch"):
                suite.integrity(root, contents)

    def test_security_gate_rejects_writable_filesystem(self):
        with tempfile.TemporaryDirectory() as work:
            root = pathlib.Path(work)
            (root / "file-00000").write_bytes(b"fixture")
            with self.assertRaisesRegex(suite.common.BenchmarkError, "write succeeded"):
                suite.security_gates(root, root)

    def test_invalid_sample_never_returns_performance(self):
        def broken(_):
            suite.require(False, "bad data")
        with self.assertRaisesRegex(suite.common.BenchmarkError, "bad data"):
            suite.sample("read", "test", "repeat", 0, [1, 2], broken, concurrency=2)

    def test_per_operation_sizes_stay_attached_to_unsorted_latencies(self):
        row = suite.sample("read", "test", "repeat", 0, [4095, 4096, 4097], lambda n: n)
        self.assertEqual(row["bytes_per_operation"], row["task_inputs"])
        self.assertEqual(row["bytes"], 4095 + 4096 + 4097)
        self.assertEqual(len(row["latencies_nanos"]), 3)
        self.assertLessEqual(row["p50_nanos"], row["p95_nanos"])

    def test_failed_run_is_incomplete_and_keeps_error(self):
        with tempfile.TemporaryDirectory() as work:
            output = pathlib.Path(work) / "result.json"
            with mock.patch.object(suite.common, "environment_metadata", return_value={}), \
                 mock.patch.object(suite, "fixture", side_effect=RuntimeError("fixture failure")):
                with self.assertRaisesRegex(RuntimeError, "fixture failure"):
                    suite.main(["--host-only", "--output", str(output)])
            report = json.loads(output.read_text())
            self.assertFalse(report["complete"])
            self.assertFalse(report["decision_eligible"])
            self.assertEqual(report["error"], "fixture failure")

    def test_registered_all_runner_preserves_profile_and_repetitions(self):
        entry = next(row for row in cli.entrypoints() if row["id"] == "filesystem-transports")
        self.assertEqual(entry["target"], "benchmarks.suites.filesystem_transports")
        arguments = all_suites.suite_arguments(entry["id"], pathlib.Path("/unused"), "standard", 5)
        args = suite.build_parser().parse_args([*arguments, "--output", "/result.json"])
        self.assertEqual((args.profile, args.repetitions), ("standard", 5))


    def test_cached_controls_cover_fd_reuse_and_all_size_boundaries(self):
        with tempfile.TemporaryDirectory() as work:
            root = pathlib.Path(work) / "source"
            contents, bulk = suite.fixture(root, "smoke")
            rows = suite.cached_read_controls(root, contents, bulk, "host", 0)
            self.assertEqual(len(rows), 18)
            for row in rows:
                self.assertEqual(row["correctness"], "passed")
                if row["operation"] in ("cached-posix-read", "cached-pathlib-read"):
                    self.assertTrue(set(suite.BOUNDARIES) <= set(row["bytes_per_operation"]))
                if row["operation"].startswith("hot-file-"):
                    self.assertEqual(set(row["bytes_per_operation"]), {256})
            path = root / "file-00000"
            path.chmod(0o600)
            path.write_bytes(b"broken")
            with self.assertRaisesRegex(suite.common.BenchmarkError, "digest differs"):
                suite.cached_read_controls(root, contents, bulk, "host", 0)
