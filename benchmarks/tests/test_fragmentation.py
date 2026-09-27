import itertools
import json
import pathlib
import unittest

from benchmarks import all as runner
from benchmarks.suites.pack.fragmentation import CACHES, CORRECTNESS, parse_samples
from benchmarks.suites.repository import BenchmarkError


class FragmentationTests(unittest.TestCase):
    def samples(self):
        return [dict(pattern=pattern, generation=generation, layout=layout, cache=cache, phase=phase,
                     cache_bytes=dict(zip(CACHES, (0, 99, 100, 101, 200)))[cache],
                     nanos=1, file_bytes=16 * 1024 * 1024, pack_target_bytes=1024 * 1024,
                     avg_chunk_bytes=256 * 1024, chunks=10, referenced_packs=2, pack_runs=2,
                     referenced_pack_bytes=200, largest_pack_bytes=100, stored_bytes=250,
                     stored_pack_bytes=200, pack_requests=10, pack_read_bytes=200,
                     chunk_range_requests=10, whole_pack_requests=0, cache_hits=0,
                     cache_promotions=0, cache_evictions=0, index_requests=0, index_bytes=0,
                     blob="matching-fixture", correctness=CORRECTNESS)
                for pattern, generation, layout, cache, phase in itertools.product(
                    ("localized", "scattered"), (0, 1, 4), ("history", "fresh"), CACHES, ("cold", "warm"))]

    def output(self, samples):
        return "\n".join("fragmentation_sample " + json.dumps(s) for s in samples) + "\ntest result: ok. 1 passed; 0 failed;"

    def test_complete_matrix(self):
        self.assertEqual(len(parse_samples(self.output(self.samples()), [0, 1, 4])), 120)

    def test_rejects_missing_duplicate_or_failed_matrix(self):
        samples = self.samples()
        for output in (self.output(samples[:-1]), self.output(samples + samples[:1]),
                       self.output(samples).replace("1 passed", "0 passed")):
            with self.assertRaises(BenchmarkError):
                parse_samples(output, [0, 1, 4])

    def test_rejects_wrong_bytes_boundary_and_counters(self):
        for field, value in (("blob", "different"), ("cache_bytes", 99),
                             ("pack_read_bytes", -1), ("correctness", "unchecked")):
            samples = self.samples()
            samples[0][field] = value
            with self.subTest(field=field), self.assertRaises(BenchmarkError):
                parse_samples(self.output(samples), [0, 1, 4])

    def test_all_uses_immutable_probe_and_smoke_profile(self):
        arguments = runner.suite_arguments("pack-fragmentation", pathlib.Path("/binaries"), "smoke", 2)
        self.assertEqual(arguments, ["--profile", "smoke", "--probe-binary", "/binaries/casita-lib-test",
                                     "--no-build", "--repetitions", "2"])
