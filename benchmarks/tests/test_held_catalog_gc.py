import json
import unittest
import pathlib
import tempfile

from benchmarks.suites.held_catalog_gc import CORRECTNESS, compaction_summary, parse_sample, sync_trace_summary
from benchmarks.suites.repository import BenchmarkError


class HeldCatalogGcTest(unittest.TestCase):
    def output(self, **changes):
        row = dict(count=128, holds=8, garbage_objects=134,
                   seconds=0.1, release_seconds=0.2, pack_bytes_before=100,
                   pack_bytes_during=120, pack_bytes_after_release=0,
                   historical_packs_preserved=True,
                   phases=[dict(phase='finish_payload_collection', seconds=0.05)],
                   correctness=CORRECTNESS)
        row.update(changes)
        return 'held_catalog_gc_sample ' + json.dumps(row) + '\ntest result: ok. 1 passed; 0 failed;'

    def test_accepts_compaction_with_retained_historical_packs(self):
        parse_sample(self.output(), 128, 8)

    def test_rejects_incomplete_gc_or_lost_historical_packs(self):
        for changes in (dict(garbage_objects=0), dict(historical_packs_preserved=False),
                        dict(pack_bytes_during=50), dict(pack_bytes_after_release=100),
                        dict(phases=[]), dict(seconds=float('nan')), dict(holds=1)):
            with self.subTest(changes=changes), self.assertRaises(BenchmarkError):
                parse_sample(self.output(**changes), 128, 8)

    def test_requires_one_completed_probe(self):
        output = self.output()
        for invalid in (output.split('\n')[0], output + '\n' + output):
            with self.assertRaises(BenchmarkError):
                parse_sample(invalid, 128, 8)


class CompactionSummaryTest(unittest.TestCase):
    def test_concurrent_intervals_are_counted_once(self):
        phases = [dict(phase="compact_pack_marker_write", seconds=2, finished_seconds=end)
                  for end in (3, 4, 8)]
        phases.append(dict(phase="finish_deletions", seconds=9, finished_seconds=9))
        self.assertEqual(compaction_summary(phases), {
            "compact_pack_marker_write": dict(calls=3, busy_seconds=5, summed_seconds=6)})

    def test_nested_intervals_and_old_binaries(self):
        self.assertEqual(compaction_summary([]), {})
        self.assertEqual(compaction_summary([
            dict(phase="compact_pack", seconds=4, finished_seconds=5),
            dict(phase="compact_pack", seconds=1, finished_seconds=3),
        ])["compact_pack"]["busy_seconds"], 4)


class SyncTraceTest(unittest.TestCase):
    def test_counts_syncs_without_confusing_rename_or_shared_ancestors(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "trace.123"
            path.write_text("\n".join([
                "123.1 fsync(5</tmp/blobs/pack-replacements/ab/file#1>) = 0 <0.002>",
                "123.2 fsync(5</tmp/blobs/pack-replacements/ab>) = 0 <0.003>",
                "123.3 fsync(5</tmp/blobs>) = 0 <0.004>",
                '123.4 rename("a", "b") = 0 <0.005>',
                "123.5 fsync(5</tmp/blobs/pack-replacements/ab>) = -1 EIO <0.006>",
                "123.6 fsync(5 <unfinished ...>",
            ]))
            result = sync_trace_summary([path])
            self.assertEqual(result["groups"]["marker_file"]["calls"], 1)
            self.assertEqual(result["groups"]["marker_directory"]["calls"], 2)
            self.assertEqual(result["groups"]["marker_directory"]["failures"], 1)
            self.assertAlmostEqual(result["groups"]["marker_directory"]["summed_seconds"], 0.009)
            self.assertEqual(result["groups"]["other"]["calls"], 1)
            self.assertEqual(result["unparsed_sync_lines"], 1)
            self.assertEqual(result["marker_leaf_directory_subset"]["calls"], 2)
