import pathlib
import subprocess
import tempfile
import unittest
from unittest import mock

from benchmarks.suites.transfer import s3_path


class S3PathTransferTests(unittest.TestCase):
    def test_rpc_budget_rejects_serialization_and_missing_counters(self):
        s3_path.check_rpc_budget({"cold": {"rpc_requests": 5}, "warm": {"rpc_requests": 5}}, 5)
        for metrics in ({"rpc_requests": 66}, {"rpc_requests": 0}, {}):
            with self.assertRaises(s3_path.bench.BenchmarkError):
                s3_path.check_rpc_budget({"cold": metrics}, 5)

    def test_publication_mode_requires_a_clean_worktree(self):
        parser = s3_path.build_parser()
        self.assertTrue(parser.parse_args(["--require-clean"]).require_clean)

    def test_rustfs_bucket_creation_uses_signed_path_style_request(self):
        response = mock.MagicMock()
        response.__enter__.return_value.status = 200
        with mock.patch.object(
            s3_path.urllib.request, "urlopen", return_value=response
        ) as urlopen:
            s3_path.create_rustfs_bucket("http://127.0.0.1:19000", "bench-bucket")
        self.assertEqual(urlopen.call_count, 2)
        request = urlopen.call_args_list[0].args[0]
        self.assertEqual(request.full_url, "http://127.0.0.1:19000/bench-bucket")
        self.assertEqual(request.method, "PUT")
        self.assertEqual(request.data, b"")
        self.assertEqual(request.get_header("Host"), "127.0.0.1:19000")
        self.assertEqual(request.get_header("Content-length"), "0")
        self.assertTrue(request.get_header("Authorization").startswith("AWS4-HMAC-SHA256"))
        readiness = urlopen.call_args_list[1].args[0]
        self.assertEqual(readiness.method, "HEAD")
        self.assertIsNone(readiness.data)

    def test_deployed_s3_url_requires_a_caller_owned_prefix(self):
        self.assertEqual(
            s3_path.split_s3_url("s3://benchmark-bucket/casita/path"),
            ("benchmark-bucket", "casita/path"),
        )
        with self.assertRaises(Exception):
            s3_path.split_s3_url("s3://benchmark-bucket")

    def test_matrix_order_is_deterministic_and_interleaved(self):
        transports = ["direct-s3", "atomic-rpc"]
        first = s3_path.interleaved_cases(
            [0, 4], [1], [0, 64], 3, [0, 30], transports
        )
        second = s3_path.interleaved_cases(
            [0, 4], [1], [0, 64], 3, [0, 30], transports
        )
        self.assertEqual(first, second)
        for repetition in range(1, 4):
            block = [case[:5] for case in first if case[5] == repetition]
            self.assertCountEqual(
                block,
                [
                    (transport, rtt, depth, 1, cache)
                    for transport in transports
                    for rtt in (0, 30)
                    for depth in (0, 4)
                    for cache in (0, 64)
                ],
            )

    def test_helper_retries_only_fastant_tsc_startup_abort(self):
        abort = subprocess.CompletedProcess(
            ["helper"],
            -6,
            "",
            "fastant-0.1.11/src/tsc_now.rs: attempt to subtract with overflow",
        )
        success = subprocess.CompletedProcess(["helper"], 0, "cold-wall-nanos 1", "")
        with mock.patch.object(s3_path.subprocess, "run", side_effect=[abort, success]) as run:
            self.assertIs(s3_path.run_helper(["helper"]), success)
            self.assertEqual(run.call_count, 2)

        ordinary_failure = subprocess.CompletedProcess(["helper"], 2, "", "bad arguments")
        with mock.patch.object(s3_path.subprocess, "run", return_value=ordinary_failure) as run:
            self.assertIs(s3_path.run_helper(["helper"]), ordinary_failure)
            self.assertEqual(run.call_count, 1)

    def test_non_negative_sweeps_reject_invalid_or_duplicate_values(self):
        self.assertEqual(s3_path.non_negative_csv("0,4,16"), [0, 4, 16])
        with self.assertRaises(Exception):
            s3_path.non_negative_csv("-1,4")
        with self.assertRaises(Exception):
            s3_path.non_negative_csv("4,4")

    def test_transport_sweep_rejects_unknown_or_duplicate_values(self):
        self.assertEqual(
            s3_path.transport_csv("direct-s3,atomic-rpc"),
            ["direct-s3", "atomic-rpc"],
        )
        with self.assertRaises(Exception):
            s3_path.transport_csv("direct-s3,direct-s3")
        with self.assertRaises(Exception):
            s3_path.transport_csv("direct-s3,magic")

    def test_tree_depth_and_selected_subtree_are_deterministic(self):
        with tempfile.TemporaryDirectory() as temporary:
            first = pathlib.Path(temporary) / "first"
            second = pathlib.Path(temporary) / "second"
            first_path = s3_path.generate_tree(first, 3, 2, 64)
            second_path = s3_path.generate_tree(second, 3, 2, 64)
            self.assertEqual(first_path, "level-0000/level-0001/level-0002/selected")
            self.assertEqual(first_path, second_path)
            self.assertEqual(
                (first / first_path / "file-000001.bin").read_bytes(),
                (second / second_path / "file-000001.bin").read_bytes(),
            )
            self.assertTrue((first / "sibling-0000.bin").is_file())
            self.assertTrue((first / "level-0000/sibling-0001.bin").is_file())

    def test_metrics_require_one_stable_snapshot_and_no_catalog_rebuild(self):
        lines = []
        for phase in ("cold", "warm"):
            values = {name: 0 for name in s3_path.REQUIRED_PHASE_METRICS}
            values.update(
                {
                    "wall_nanos": 100,
                    "published_objects": 2,
                    "payloads_sent": 2,
                    "chunks_sent": 2,
                    "pack_chunk_range_requests": 3,
                    "wal_manifest_refresh_requests": 1,
                    "wal_cache_hits": 1,
                }
            )
            lines.extend(
                f"{phase}-{name.replace('_', '-')} {value}" for name, value in values.items()
            )
        parsed = s3_path.parse_metrics("\n".join(lines))
        self.assertEqual(parsed["cold"]["pack_chunk_range_requests"], 3)
        self.assertEqual(parsed["warm"]["wal_manifest_refresh_requests"], 1)

        broken = "\n".join(lines).replace(
            "cold-wal-manifest-refresh-requests 1",
            "cold-wal-manifest-refresh-requests 2",
        )
        with self.assertRaises(Exception):
            s3_path.parse_metrics(broken)

        atomic = "\n".join(lines).replace(
            "wal-manifest-refresh-requests 1",
            "wal-manifest-refresh-requests 0",
        )
        parsed = s3_path.parse_metrics(atomic, expected_wal_refreshes=0)
        self.assertEqual(parsed["cold"]["wal_manifest_refresh_requests"], 0)

    def test_report_keeps_depth_cache_and_phase_visible(self):
        metrics = {name: 0 for name in s3_path.REQUIRED_PHASE_METRICS}
        metrics.update(
            {
                "wall_nanos": 1_000_000,
                "pack_chunk_range_requests": 5,
                "pack_chunk_range_bytes": 1024,
                "wal_manifest_refresh_requests": 1,
            }
        )
        report = s3_path.render_report(
            {
                "configuration": {
                    "endpoint": "s3://bucket/prefix",
                    "latency_label": "us-east-1",
                },
                "remote_prefixes": ["s3://bucket/prefix/run/sample"],
                "samples": [
                    {
                        "transport": "direct-s3",
                        "rtt_ms": 0,
                        "depth": 4,
                        "subtree_files": 1,
                        "cache_mib": 0,
                        "cold": metrics,
                        "warm": metrics,
                    },
                    {
                        "transport": "direct-s3",
                        "rtt_ms": 80,
                        "depth": 4,
                        "subtree_files": 1,
                        "cache_mib": 0,
                        "cold": metrics,
                        "warm": metrics,
                    },
                ]
            }
        )
        self.assertIn("| direct-s3 | 0 ms | 4 | 1 | 0 MiB | cold |", report)
        self.assertIn("| direct-s3 | 0 ms | 4 | 1 | 0 MiB | warm |", report)
        self.assertIn("s3://bucket/prefix", report)
        self.assertIn("same endpoint", report)
        self.assertIn("intentionally retained", report)
        self.assertIn("## RTT sensitivity", report)
        self.assertIn("Fitted turns", report)
        self.assertIn("Independent selected-subtree payload reads", report)
        self.assertNotIn("With atomic RPC", report)
