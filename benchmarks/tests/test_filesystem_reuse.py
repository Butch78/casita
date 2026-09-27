import json
from pathlib import Path
import tempfile
import unittest

from benchmarks.suites.filesystem_reuse import trace_summary


class FilesystemReuseTests(unittest.TestCase):
    def test_trace_counts_cache_results_and_keeps_overlapping_phases_separate(self):
        events = [
            {"target": "casita::filesystem::cache", "fields": {"hits": 12, "misses": 0}},
            {"target": "casita::pin_timing", "fields": {"phase": "journal_flush", "elapsed_seconds": 0.2}},
            {"target": "casita::pin_timing", "fields": {"phase": "journal_append_sync", "elapsed_seconds": 0.1}},
            {"target": "casita::repository::filesystem", "span": {"name": "repository.import_path.walk"},
             "fields": {"message": "close", "time.busy": "500ms", "time.idle": "250µs"}},
        ]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "trace"
            path.write_text("\n".join(json.dumps(event) for event in events))
            result = trace_summary(path)
        self.assertEqual(result["cache_hits"], 12)
        self.assertEqual(result["cache_misses"], 0)
        self.assertEqual(result["pin_phases"]["journal_flush"], [1, 0.2])
        self.assertEqual(result["pin_phases"]["journal_append_sync"], [1, 0.1])
        self.assertAlmostEqual(result["spans"]["repository.import_path.walk"][1], 0.50025)

    def test_incomplete_trace_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "trace"
            path.write_text("")
            with self.assertRaisesRegex(ValueError, "completed filesystem import"):
                trace_summary(path)


if __name__ == "__main__":
    unittest.main()
