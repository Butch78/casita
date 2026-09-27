import unittest

from benchmarks.suites.pack import s3_index as s3_pack_index


class S3PackIndexTests(unittest.TestCase):
    def test_metrics_require_state_seeded_cold_and_warm_hits(self):
        text = (
            "pack-count 3\n"
            "cold-wall-nanos 20\ncold-command-nanos 24\n"
            "cold-first-snapshot-nanos 4\ncold-repeat-snapshot-nanos 2\n"
            "cold-payload-open-nanos 12\n"
            "cold-state-open-nanos 11\ncold-list-requests 0\n"
            "cold-footer-range-requests 0\ncold-footer-range-bytes 0\n"
            "cold-index-hits 1\ncold-index-fallbacks 0\n"
            "cold-index-pointer-requests 0\ncold-index-put-requests 0\n"
            "cold-index-put-bytes 0\n"
            "cold-index-sharded-base 0\n"
            "cold-index-checkpoint-base 0\n"
            "cold-index-run-objects 0\n"
            "warm-wall-nanos 10\nwarm-command-nanos 13\n"
            "warm-first-snapshot-nanos 3\nwarm-repeat-snapshot-nanos 2\n"
            "warm-payload-open-nanos 8\n"
            "warm-state-open-nanos 7\nwarm-list-requests 0\n"
            "warm-footer-range-requests 0\nwarm-footer-range-bytes 0\n"
            "warm-index-pointer-requests 0\nwarm-index-requests 0\n"
            "warm-index-bytes 120\n"
            "warm-index-hash-nanos 2\nwarm-index-decode-nanos 3\n"
            "warm-index-hits 1\nwarm-index-fallbacks 0\n"
            "warm-index-put-requests 0\n"
            "warm-index-sharded-base 0\n"
            "warm-index-checkpoint-base 0\n"
            "warm-index-run-objects 0\n"
            "warm-open-wal-writer-open-requests 1\n"
            "warm-open-wal-writer-open-nanos 4\n"
            "warm-open-wal-manifest-load-requests 0\n"
            "warm-open-wal-fragment-get-requests 2\n"
            "warm-open-wal-fragment-get-bytes 150\n"
            "warm-open-wal-fragment-records 4\n"
            "warm-open-wal-fragment-record-bytes 140\n"
            "warm-open-wal-fragment-get-nanos 2\n"
            "warm-open-wal-parquet-parse-nanos 1\n"
            "warm-open-wal-state-decode-nanos 1\n"
            "warm-open-wal-fragment-put-requests 0\n"
            "warm-open-wal-manifest-put-requests 0\n"
            "warm-open-wal-checkpoint-bytes 100\n"
            "warm-open-wal-checkpoint-objects 8\n"
            "warm-open-wal-checkpoint-roots 1\n"
            "warm-open-wal-tail-deltas 1\n"
            "warm-first-snapshot-wal-manifest-refresh-requests 1\n"
            "warm-first-snapshot-wal-manifest-refresh-nanos 2\n"
            "warm-first-snapshot-wal-manifest-load-requests 0\n"
            "warm-first-snapshot-wal-fragment-get-requests 0\n"
            "warm-first-snapshot-wal-cache-hits 1\n"
            "warm-first-snapshot-wal-cache-misses 0\n"
            "warm-first-snapshot-wal-fragment-put-requests 0\n"
            "warm-first-snapshot-wal-manifest-put-requests 0\n"
            "warm-repeat-snapshot-wal-manifest-refresh-requests 1\n"
            "warm-repeat-snapshot-wal-manifest-refresh-nanos 1\n"
            "warm-repeat-snapshot-wal-fragment-get-requests 0\n"
            "warm-repeat-snapshot-wal-cache-hits 1\n"
            "warm-repeat-snapshot-wal-fragment-put-requests 0\n"
            "warm-repeat-snapshot-wal-manifest-put-requests 0\n"
        )
        metrics = s3_pack_index.parse_metrics(text)
        self.assertEqual(metrics["pack_count"], 3)
        self.assertEqual(metrics["warm_index_hits"], 1)
        checks = s3_pack_index.request_budget_checks(metrics)
        self.assertEqual({check["status"] for check in checks}, {"passed"})
        self.assertEqual(
            {check["id"]: check["limit"] for check in checks},
            {
                "warm-catalog-get-requests": 0,
                "warm-list-requests": 0,
                "warm-footer-range-requests": 0,
                "warm-open-total-requests": 3,
                "warm-first-snapshot-total-requests": 1,
                "warm-repeat-snapshot-total-requests": 1,
            },
        )
        invalid = (
            ("warm-footer-range-requests 0", "warm-footer-range-requests 1"),
            ("warm-list-requests 0", "warm-list-requests 1"),
            ("warm-index-pointer-requests 0", "warm-index-pointer-requests 1"),
        )
        for original, replacement in invalid:
            with self.subTest(replacement=replacement), self.assertRaises(
                s3_pack_index.bench.BenchmarkError
            ):
                s3_pack_index.parse_metrics(text.replace(original, replacement))

    def test_report_groups_repetitions(self):
        metrics = {
            "pack_count": 4,
            "cold_wall_nanos": 20_000_000,
            "warm_wall_nanos": 10_000_000,
            "warm_command_nanos": 13_000_000,
            "warm_first_snapshot_nanos": 3_000_000,
            "warm_repeat_snapshot_nanos": 2_000_000,
            "warm_payload_open_nanos": 8_000_000,
            "warm_state_open_nanos": 7_000_000,
            "cold_footer_range_requests": 0,
            "warm_footer_range_requests": 0,
            "warm_list_requests": 0,
            "warm_index_pointer_requests": 0,
            "warm_index_requests": 0,
            "warm_index_put_requests": 0,
            "warm_index_sharded_base": 0,
            "warm_index_checkpoint_base": 0,
            "warm_index_run_objects": 0,
            "cold_footer_range_bytes": 0,
            "warm_index_bytes": 4608,
            "warm_index_hash_nanos": 2_000_000,
            "warm_index_decode_nanos": 3_000_000,
            "warm_open_wal_checkpoint_objects": 32,
            "warm_open_wal_checkpoint_bytes": 4096,
            "warm_open_wal_tail_deltas": 1,
            "warm_open_wal_writer_open_nanos": 2_000_000,
            "warm_open_wal_writer_open_requests": 1,
            "warm_open_wal_manifest_load_requests": 0,
            "warm_open_wal_fragment_get_nanos": 1_000_000,
            "warm_open_wal_fragment_get_requests": 2,
            "warm_open_wal_fragment_put_requests": 0,
            "warm_open_wal_manifest_put_requests": 0,
            "warm_open_wal_fragment_get_bytes": 4608,
            "warm_open_wal_fragment_records": 4,
            "warm_open_wal_fragment_record_bytes": 4352,
            "warm_open_wal_parquet_parse_nanos": 500_000,
            "warm_open_wal_state_decode_nanos": 250_000,
            "warm_first_snapshot_wal_manifest_refresh_nanos": 300_000,
            "warm_first_snapshot_wal_manifest_refresh_requests": 1,
            "warm_first_snapshot_wal_manifest_load_requests": 0,
            "warm_first_snapshot_wal_fragment_get_requests": 0,
            "warm_first_snapshot_wal_fragment_put_requests": 0,
            "warm_first_snapshot_wal_manifest_put_requests": 0,
            "warm_repeat_snapshot_wal_manifest_refresh_nanos": 200_000,
            "warm_repeat_snapshot_wal_manifest_refresh_requests": 1,
            "warm_repeat_snapshot_wal_manifest_load_requests": 0,
            "warm_repeat_snapshot_wal_fragment_get_requests": 0,
            "warm_repeat_snapshot_wal_fragment_put_requests": 0,
            "warm_repeat_snapshot_wal_manifest_put_requests": 0,
        }
        budgets = {
            "warm_catalog_get_requests": 0,
            "warm_list_requests": 0,
            "warm_footer_range_requests": 0,
            "warm_open_total_requests": 3,
            "warm_first_snapshot_total_requests": 1,
            "warm_repeat_snapshot_total_requests": 1,
        }
        checks = s3_pack_index.request_budget_checks(metrics, budgets)
        samples = [
            {
                "files": 32,
                "target_mib": 16,
                "metrics": metrics,
                "budget_checks": checks,
            }
        ]
        report = s3_pack_index.render_report(
            {
                "budgets": budgets,
                "budget_summary": s3_pack_index.summarize_request_budgets(samples),
                "samples": samples,
            }
        )
        self.assertIn(
            "| 32 | 16 MiB | 1 | 4 | inline | 0.0200 s | 0.0100 s | 2.00x | 0.0130 s | 0.0030 s | 0.0020 s | 0.0080 s | 0.0070 s | 0 | 0 |",
            report,
        )
        self.assertIn(
            "| 32 | 16 MiB | 32 | 4.0 KiB | 1 | 4 | 4.2 KiB | 7.000 ms | 2.000 ms | 1.000 ms | 4.5 KiB | 0.500 ms | 0.250 ms | 0.300 ms | 0.200 ms |",
            report,
        )
        self.assertIn("Overall status: **passed**", report)
        self.assertIn("| 32 | 16 MiB | 3 | 0 | 1 | 2 | 1 | 1 |", report)

    def test_sharded_open_has_one_map_get_and_one_more_total_request(self):
        metrics = {
            "warm_index_sharded_base": 1,
            "warm_index_pointer_requests": 0,
            "warm_index_requests": 1,
            "warm_list_requests": 0,
            "warm_footer_range_requests": 0,
            "warm_index_put_requests": 0,
            "warm_open_wal_writer_open_requests": 1,
            "warm_open_wal_manifest_load_requests": 0,
            "warm_open_wal_manifest_refresh_requests": 0,
            "warm_open_wal_fragment_get_requests": 2,
            "warm_open_wal_fragment_put_requests": 0,
            "warm_open_wal_manifest_put_requests": 0,
            "warm_first_snapshot_wal_manifest_load_requests": 0,
            "warm_first_snapshot_wal_manifest_refresh_requests": 1,
            "warm_first_snapshot_wal_fragment_get_requests": 0,
            "warm_first_snapshot_wal_fragment_put_requests": 0,
            "warm_first_snapshot_wal_manifest_put_requests": 0,
            "warm_repeat_snapshot_wal_manifest_load_requests": 0,
            "warm_repeat_snapshot_wal_manifest_refresh_requests": 1,
            "warm_repeat_snapshot_wal_fragment_get_requests": 0,
            "warm_repeat_snapshot_wal_fragment_put_requests": 0,
            "warm_repeat_snapshot_wal_manifest_put_requests": 0,
        }
        budgets = {
            "warm_catalog_get_requests": 0,
            "warm_sharded_catalog_get_requests": 1,
            "warm_list_requests": 0,
            "warm_footer_range_requests": 0,
            "warm_open_total_requests": 3,
            "warm_sharded_open_total_requests": 4,
            "warm_first_snapshot_total_requests": 1,
            "warm_repeat_snapshot_total_requests": 1,
        }
        checks = s3_pack_index.request_budget_checks(metrics, budgets)
        self.assertEqual({check["status"] for check in checks}, {"passed"})
        self.assertIn(
            "warm-sharded-catalog-get-requests",
            {check["id"] for check in checks},
        )
        self.assertIn(
            "warm-sharded-open-total-requests",
            {check["id"] for check in checks},
        )

    def test_catalog_layout_distinguishes_checkpoint_and_runs(self):
        self.assertEqual(
            s3_pack_index.catalog_layout(
                [
                    {
                        "warm_index_sharded_base": 0,
                        "warm_index_checkpoint_base": 1,
                        "warm_index_run_objects": 2,
                    }
                ]
            ),
            "checkpoint+2 runs",
        )


if __name__ == "__main__":
    unittest.main()
