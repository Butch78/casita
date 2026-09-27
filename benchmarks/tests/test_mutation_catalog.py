import json
import unittest

from benchmarks.suites.mutation_catalog import CORRECTNESS, parse_sample
from benchmarks.suites.repository import BenchmarkError


class MutationCatalogTest(unittest.TestCase):
    def output(self, **changes):
        sample = dict(count=65, nanos=100, peak_catalog_bytes=1024 * 1024,
                      correctness=CORRECTNESS)
        sample.update(changes)
        return 'catalog_history_sample ' + json.dumps(sample) + '\ntest result: ok. 1 passed; 0 failed;'

    def test_accepts_bounded_success(self):
        self.assertEqual(parse_sample(self.output(), 65)['count'], 65)

    def test_rejects_accumulated_catalogs(self):
        with self.assertRaises(BenchmarkError):
            parse_sample(self.output(peak_catalog_bytes=65 * 1024 * 1024), 65)

    def test_rejects_wrong_case(self):
        with self.assertRaises(BenchmarkError):
            parse_sample(self.output(count=63), 65)

    def test_rejects_missing_gate(self):
        with self.assertRaises(BenchmarkError):
            parse_sample(self.output().split('\n')[0], 65)
