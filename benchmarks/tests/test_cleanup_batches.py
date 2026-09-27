import json
import unittest

from benchmarks.suites.cleanup_batches import CORRECTNESS, QUEUE_CORRECTNESS, parse_sample, parse_queue_sample
from benchmarks.suites.repository import BenchmarkError


class CleanupBatchesTest(unittest.TestCase):
    def output(self, **changes):
        sample = dict(count=1001, nanos=100, claim_pairs=2,
                      correctness=CORRECTNESS)
        sample.update(changes)
        return 'cleanup_batch_sample ' + json.dumps(sample) + '\ntest result: ok. 1 passed; 0 failed;'

    def test_accepts_bounded_success(self):
        self.assertEqual(parse_sample(self.output(), 1001)['count'], 1001)

    def test_rejects_per_file_claims(self):
        with self.assertRaises(BenchmarkError):
            parse_sample(self.output(claim_pairs=1001), 1001)

    def test_rejects_wrong_case(self):
        with self.assertRaises(BenchmarkError):
            parse_sample(self.output(count=999), 1001)

    def test_rejects_missing_gate(self):
        with self.assertRaises(BenchmarkError):
            parse_sample(self.output().split('\n')[0], 1001)


class RetirementQueueTest(unittest.TestCase):
    def output(self, **changes):
        sample = dict(count=100000, nanos=100, rss_before=1024,
                      rss_at_first_delete=2048, correctness=QUEUE_CORRECTNESS)
        sample.update(changes)
        return 'retirement_queue_sample ' + json.dumps(sample) + '\ntest result: ok. 1 passed; 0 failed;'

    def test_accepts_queue_sample(self):
        self.assertEqual(parse_queue_sample(self.output(), 100000)['rss_at_first_delete'], 2048)

    def test_accepts_unavailable_rss(self):
        parse_queue_sample(self.output(rss_before=None, rss_at_first_delete=None), 100000)

    def test_rejects_invalid_rss(self):
        with self.assertRaises(BenchmarkError):
            parse_queue_sample(self.output(rss_at_first_delete=-1), 100000)

    def test_requires_completed_probe(self):
        with self.assertRaises(BenchmarkError):
            parse_queue_sample(self.output().split('\n')[0], 100000)
