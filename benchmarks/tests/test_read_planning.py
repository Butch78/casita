import copy
import unittest

from benchmarks.suites.pack import read_planning as model


def chunk(digest, offset, size=256, pack="p"):
    return dict(digest=digest, offset=offset, size=size, framed_len=size,
                pack=pack, pack_len=4096)


class ReadPlanningTests(unittest.TestCase):
    def test_merge_preserves_pack_boundaries_and_deduplicates(self):
        a, b, c = chunk("a", 0), chunk("b", 256), chunk("c", 0, pack="q")
        ranges = model.requests([b, c, a, a], 0, 1024)
        self.assertEqual([(r["pack"], r["start"], r["end"]) for r in ranges],
                         [("p", 0, 512), ("q", 0, 256)])

    def test_cumulative_overfetch_is_bounded(self):
        chunks = [chunk("a", 0), chunk("b", 356), chunk("c", 712), chunk("d", 1068)]
        ranges = model.requests(chunks, 128, 4096)
        self.assertEqual(len(ranges), 2)
        self.assertEqual([len(r["chunks"]) for r in ranges], [2, 2])

    def test_oversized_chunk_is_alone(self):
        chunks = [chunk("a", 0, 513), chunk("b", 513, 1)]
        self.assertEqual(list(model.windows(chunks, 512)), [[chunks[0]], [chunks[1]]])
        sample = model.replay(chunks, 512, 0, model.Cache(0), "sequential")
        self.assertEqual(sample["requests"], 2)
        self.assertEqual(sample["max_decoded_window"], 513)

    def test_coverage_uses_the_whole_known_group(self):
        chunks = [chunk("a", 0, 128), chunk("b", 256, 128), chunk("c", 384, 256)]
        self.assertEqual(len(model.requests(chunks, 128, 1024)), 2)
        self.assertEqual(len(model.requests(chunks, None, 1024)), 1)

    def test_compressed_window_requires_separate_decode_budget(self):
        chunks = [dict(chunk(str(i), i*128, 1024), framed_len=128) for i in range(3)]
        decoded = model.replay(chunks, 384, None, model.Cache(0), "sequential")
        compressed = model.replay(chunks, 384, None, model.Cache(0), "sequential", "compressed")
        self.assertEqual(decoded["requests"], 3)
        self.assertEqual(compressed["requests"], 1)
        self.assertEqual(compressed["max_compressed_window"], 384)
        self.assertEqual(compressed["max_decoded_window"], 3072)

    def test_eager_seek_exposes_unused_fetches(self):
        chunks = [chunk(str(i), i*256) for i in range(8)]
        sample = model.replay(chunks, 1024, 0, model.Cache(0), "seek-one")
        self.assertEqual(sample["consumed_bytes"], 256)
        self.assertEqual(sample["fetched_bytes"], 1024)
        self.assertEqual(sample["requests"], 1)

    def test_lru_fit_boundary_and_interleaved_readers(self):
        chunks = [chunk("a", 0), chunk("b", 256)]
        for capacity, expected_warm in [(511, 2), (512, 0), (513, 0)]:
            cache = model.Cache(capacity)
            model.replay(chunks, 256, 0, cache, "sequential")
            sample = model.replay(chunks, 256, 0, cache, "sequential")
            self.assertEqual(sample["requests"], expected_warm)
            self.assertLessEqual(cache.peak, capacity)
        sample = model.replay(chunks, 256, 0, model.Cache(512), "interleaved-scans")
        self.assertEqual(sample["consumed_bytes"], 1024)
        self.assertEqual(sample["requests"], 2)

    def test_trace_rejects_bad_inventory_and_overlapping_locations(self):
        item = dict(read_plan=[chunk("a", 0), chunk("b", 256)], chunks=2,
                    file_bytes=512, referenced_packs=1, largest_pack_bytes=4096,
                    referenced_pack_bytes=4096)
        model.validate_trace(item)
        for mutation in (lambda t: t.update(file_bytes=511),
                         lambda t: t["read_plan"][1].update(offset=128),
                         lambda t: t["read_plan"][0].update(pack_len=100)):
            invalid = copy.deepcopy(item)
            mutation(invalid)
            with self.assertRaises(ValueError):
                model.validate_trace(invalid)

    def test_permanent_threshold_cases(self):
        samples = model.boundary_cases()
        self.assertEqual(len(samples), 12)
        self.assertEqual({s["boundary"] for s in samples}, {"window", "gap", "overfetch", "coverage"})
