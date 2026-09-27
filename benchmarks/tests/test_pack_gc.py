import pathlib
import tempfile
import unittest

from benchmarks.suites.pack import gc as pack_gc


class PackGcTests(unittest.TestCase):
    def test_evenly_distributed_indexes_are_exact_and_unique(self):
        self.assertEqual(pack_gc.evenly_distributed_indexes(10, 1), [0])
        self.assertEqual(pack_gc.evenly_distributed_indexes(10, 50), [0, 2, 4, 6, 8])
        self.assertEqual(pack_gc.evenly_distributed_indexes(10, 100), list(range(10)))

    def test_pack_and_replacement_inventories_decode_wire_layout(self):
        with tempfile.TemporaryDirectory() as temporary:
            repository = pathlib.Path(temporary)
            pack = repository / "blobs/packs/b3/aa"
            pack.mkdir(parents=True)
            footer = (1).to_bytes(8, "little") + bytes(pack_gc.PACK_ENTRY_BYTES)
            trailer = len(footer).to_bytes(8, "little") + pack_gc.PACK_MAGIC
            (pack / ("11" * 32)).write_bytes(b"body" + footer + trailer)
            marker = repository / "blobs/pack-replacements/b3/bb"
            marker.mkdir(parents=True)
            (marker / ("33" * 32)).write_bytes(
                pack_gc.REPLACEMENT_MAGIC
                + bytes.fromhex("11" * 32)
                + b"\x01"
                + bytes.fromhex("22" * 32)
            )
            tombstone = repository / "blobs/pack-tombstones/b3/cc"
            tombstone.mkdir(parents=True)
            (tombstone / ("44" * 32)).write_bytes(
                pack_gc.TOMBSTONE_MAGIC
                + bytes.fromhex("11" * 32)
                + (1).to_bytes(8, "little")
                + b"\x01"
            )
            (tombstone / ("77" * 32)).write_bytes(
                b"casitad1"
                + (2).to_bytes(8, "little")
                + bytes.fromhex("55" * 32)
                + (2).to_bytes(8, "little")
                + b"\x02"
                + bytes.fromhex("66" * 32)
                + (1).to_bytes(8, "little")
                + b"\x01"
            )
            packs = pack_gc.pack_inventory(repository)
            replacements = pack_gc.replacement_inventory(repository)
            tombstones = pack_gc.tombstone_inventory(repository)
            self.assertEqual(packs["11" * 32]["entries"], 1)
            self.assertEqual(packs["11" * 32]["body_bytes"], 4)
            self.assertEqual(replacements, {"11" * 32: "22" * 32})
            self.assertEqual(
                tombstones,
                {"11" * 32: {0}, "55" * 32: {1}, "66" * 32: {0}},
            )

    def test_gc_summary_checks_request_shape_and_amplification(self):
        before = {
            "aa": {"bytes": 110, "body_bytes": 100, "entries": 10},
            "bb": {"bytes": 55, "body_bytes": 50, "entries": 5},
        }
        after = {"cc": {"bytes": 77, "body_bytes": 70, "entries": 7}}
        summary = pack_gc.summarize_gc(
            before,
            after,
            {"aa": "cc", "bb": None},
            {},
            {
                "pack_whole_requests": 1,
                "pack_whole_bytes": 110,
                "pack_gc_replacement_put_requests": 1,
                "pack_gc_replacement_put_bytes": 77,
                "pack_gc_marker_put_requests": 2,
                "pack_gc_delete_requests": 2,
            },
        )
        self.assertEqual(summary["dirty_packs"], 2)
        self.assertEqual(summary["dead_packs"], 1)
        self.assertEqual(summary["rewritten_packs"], 1)
        self.assertEqual(summary["removed_entries"], 8)
        self.assertAlmostEqual(summary["read_amplification"], 110 / 80)
        self.assertAlmostEqual(summary["write_amplification"], 77 / 80)
        with self.assertRaises(Exception):
            pack_gc.summarize_gc(
                before,
                after,
                {"aa": "cc", "bb": None},
                {},
                {
                    "pack_whole_requests": 2,
                    "pack_gc_replacement_put_requests": 1,
                    "pack_gc_replacement_put_bytes": 77,
                    "pack_gc_marker_put_requests": 2,
                    "pack_gc_delete_requests": 2,
                },
            )

    def test_report_contains_density_and_amplification(self):
        sample = {
            "target_mib": 4,
            "requested_dead_percent": 10,
            "wall_seconds": 1.25,
            "gc_metrics": {
                "actual_dead_percent": 12.5,
                "dirty_packs": 2,
                "dead_packs": 1,
                "rewritten_packs": 1,
                "deferred_packs": 0,
                "tombstone_put_requests": 0,
                "whole_read_bytes": 1024,
                "replacement_write_bytes": 512,
                "tombstone_write_bytes": 0,
                "reclaimed_pack_bytes": 256,
                "read_amplification": 4.0,
                "write_amplification": 2.0,
            },
        }
        report = pack_gc.render_report({"samples": [sample]})
        self.assertIn("| 4 MiB | 10% | 12.5% |", report)
        self.assertIn("4.00x | 2.00x", report)


if __name__ == "__main__":
    unittest.main()
