import unittest

from benchmarks.suites.pack import s3 as s3_pack


class S3PackTests(unittest.TestCase):
    def test_integer_sweeps_validate_bounds_and_duplicates(self):
        self.assertEqual(s3_pack.positive_csv("4,16,32"), [4, 16, 32])
        self.assertEqual(s3_pack.positive_csv("0,64", allow_zero=True), [0, 64])
        with self.assertRaises(Exception):
            s3_pack.positive_csv("0")
        with self.assertRaises(Exception):
            s3_pack.positive_csv("4,4")

    def test_report_keeps_source_request_counters(self):
        result = {
            "configuration": {"latency_label": "25ms"},
            "samples": [
                {
                    "target_mib": 16,
                    "cache_mib": 64,
                    "wall_seconds": 1.25,
                    "operation_metrics": {
                        "source_pack_chunk_range_requests": 1,
                        "source_pack_whole_requests": 2,
                        "source_pack_cache_hits": 30,
                        "source_pack_footer_range_requests": 4,
                        "source_pack_footer_range_bytes": 8192,
                    },
                }
            ],
        }
        report = s3_pack.render_report(result)
        self.assertIn("`25ms`", report)
        self.assertIn("| 16 MiB | 64 MiB | 1 |", report)
        self.assertIn("| 1 | 2 | 30 | 4 | 8.0 KiB |", report)


if __name__ == "__main__":
    unittest.main()
