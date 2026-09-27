import pathlib
import tempfile
import unittest
from unittest import mock
from urllib.error import URLError

from benchmarks.suites.pack import s3_gc as s3_pack_gc


class S3PackGcTests(unittest.TestCase):
    def test_rustfs_allows_slow_startup_outside_measurements(self):
        process = mock.Mock()
        process.poll.return_value = None
        ready = mock.MagicMock()
        ready.__enter__.return_value.status = 200
        with tempfile.TemporaryDirectory() as temporary, \
                mock.patch.object(s3_pack_gc.shutil, 'which', return_value='/fake/rustfs'), \
                mock.patch.object(s3_pack_gc.subprocess, 'Popen', return_value=process), \
                mock.patch.object(s3_pack_gc.time, 'monotonic', side_effect=[0,0,11]), \
                mock.patch.object(s3_pack_gc.time, 'sleep'), \
                mock.patch.object(s3_pack_gc.urllib.request, 'urlopen', side_effect=[URLError('warming'),ready]):
            server = s3_pack_gc.Rustfs(pathlib.Path(temporary)/'rustfs', 12345, 12346)
            self.assertIs(server.process, process)
            process.terminate.assert_not_called()

    def test_rustfs_startup_failure_preserves_diagnostics(self):
        process = mock.Mock()
        process.poll.return_value = 1
        def launch(*args, **kwargs):
            kwargs['stdout'].write(b'failed to initialize storage')
            return process
        with tempfile.TemporaryDirectory() as temporary, \
                mock.patch.object(s3_pack_gc.shutil, 'which', return_value='/fake/rustfs'), \
                mock.patch.object(s3_pack_gc.subprocess, 'Popen', side_effect=launch):
            with self.assertRaisesRegex(s3_pack_gc.bench.BenchmarkError, 'failed to initialize storage'):
                s3_pack_gc.Rustfs(pathlib.Path(temporary)/'rustfs', 12345, 12346)

    @staticmethod
    def metrics_text(*, chunk_ranges: int = 0) -> str:
        return (
            "gc-wall-nanos 10\nremoved-payloads 0\nremoved-chunks 3\n"
            "pack-list-requests 0\npack-gc-manifest-list-requests 0\n"
            "pack-gc-loose-chunk-list-requests 0\npack-footer-range-requests 0\n"
            f"pack-chunk-range-requests {chunk_ranges}\n"
            "pack-whole-requests 2\npack-whole-bytes 100\n"
            "pack-gc-replacement-put-requests 2\npack-gc-replacement-put-bytes 80\n"
            "pack-gc-marker-put-requests 3\npack-gc-delete-requests 3\n"
            "pack-gc-manifest-delete-requests 0\npack-gc-outboard-delete-requests 0\n"
            "pack-gc-loose-chunk-delete-requests 0\n"
            "pack-gc-tombstone-put-requests 1\npack-gc-tombstone-put-bytes 49\n"
            "pack-gc-tombstone-delete-requests 0\npack-gc-deferred-packs 1\n"
            "pack-index-pointer-requests 0\npack-index-requests 0\n"
            "pack-index-put-requests 0\n"
            "gc-wal-writer-open-requests 0\ngc-wal-manifest-load-requests 1\n"
            "gc-wal-manifest-refresh-requests 1\ngc-wal-fragment-get-requests 1\n"
            "gc-wal-fragment-put-requests 1\ngc-wal-manifest-put-requests 1\n"
            "gc-wal-logical-shard-get-requests 0\n"
            "gc-wal-logical-shard-put-requests 2\n"
            "gc-wal-logical-shard-barrier-get-requests 2\n"
            "gc-wal-logical-shard-barrier-put-requests 3\n"
            "gc-wal-logical-shard-inventory-list-requests 0\n"
            "gc-wal-logical-shard-delete-requests 0\n"
        )

    def test_metrics_require_gc_and_pack_counters(self):
        metrics = s3_pack_gc.parse_metrics(self.metrics_text())
        self.assertEqual(metrics["gc_wall_nanos"], 10)
        self.assertEqual(metrics["pack_whole_requests"], 2)
        self.assertEqual(s3_pack_gc.request_ledger(metrics)["total"], 23)
        self.assertEqual(
            {check["status"] for check in s3_pack_gc.request_budget_checks(metrics)},
            {"passed"},
        )
        with self.assertRaises(Exception):
            s3_pack_gc.parse_metrics("gc-wall-nanos 10\n")
        with self.assertRaises(Exception):
            s3_pack_gc.parse_metrics(self.metrics_text(chunk_ranges=1))

    def test_report_groups_repetitions(self):
        sample = {
            "target_mib": 16,
            "requested_dead_percent": 10,
            "wall_seconds": 0.5,
            "metrics": {
                "pack_whole_requests": 2,
                "pack_whole_bytes": 4096,
                "pack_gc_replacement_put_requests": 2,
                "pack_gc_replacement_put_bytes": 2048,
                "pack_gc_delete_requests": 3,
                "pack_list_requests": 0,
                "pack_gc_manifest_list_requests": 0,
                "pack_gc_loose_chunk_list_requests": 0,
                "pack_footer_range_requests": 0,
                "pack_chunk_range_requests": 0,
                "pack_index_pointer_requests": 0,
                "pack_index_requests": 0,
                "pack_index_put_requests": 0,
                "pack_gc_marker_put_requests": 3,
                "pack_gc_manifest_delete_requests": 0,
                "pack_gc_outboard_delete_requests": 0,
                "pack_gc_loose_chunk_delete_requests": 0,
                "pack_gc_deferred_packs": 1,
                "pack_gc_tombstone_put_requests": 1,
                "pack_gc_tombstone_put_bytes": 49,
                "pack_gc_tombstone_delete_requests": 0,
                "gc_wal_writer_open_requests": 0,
                "gc_wal_manifest_load_requests": 1,
                "gc_wal_manifest_refresh_requests": 1,
                "gc_wal_fragment_get_requests": 1,
                "gc_wal_fragment_put_requests": 1,
                "gc_wal_manifest_put_requests": 1,
                "gc_wal_logical_shard_get_requests": 0,
                "gc_wal_logical_shard_put_requests": 2,
                "gc_wal_logical_shard_barrier_get_requests": 2,
                "gc_wal_logical_shard_barrier_put_requests": 3,
                "gc_wal_logical_shard_inventory_list_requests": 0,
                "gc_wal_logical_shard_delete_requests": 0,
                "removed_chunks": 12,
            },
        }
        budgets = {
            "gc_wal_requests": 12,
            "gc_inventory_list_requests": 0,
            "gc_catalog_put_requests": 0,
            "gc_survivor_range_requests": 0,
            "gc_unexpected_catalog_read_requests": 0,
            "max_gc_tombstone_put_requests": 1,
            "gc_logical_shard_barrier_get_requests": 2,
            "gc_logical_shard_barrier_put_requests": 3,
            "gc_logical_shard_inventory_list_requests": 0,
            "max_gc_logical_shard_put_requests": 2,
        }
        sample["budget_checks"] = s3_pack_gc.request_budget_checks(sample["metrics"], budgets)
        samples = [sample, sample]
        report = s3_pack_gc.render_report(
            {
                "samples": samples,
                "budgets": budgets,
                "budget_summary": s3_pack_gc.summarize_request_budgets(samples),
            }
        )
        self.assertIn("| 16 MiB | 10% | 2 |", report)
        self.assertIn("| 2 | 4.0 KiB | 2 | 2.0 KiB | 1 | 1 | 49.0 B | 3 | 12 |", report)
        self.assertIn("## Complete GC request ledger", report)
        self.assertIn("Overall status: **passed**", report)


if __name__ == "__main__":
    unittest.main()
