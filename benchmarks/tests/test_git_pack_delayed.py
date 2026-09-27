import copy
import itertools
import unittest

from benchmarks.suites.git_pack_delayed import MODES, check_samples
from benchmarks import all as runner


class DelayedPackTests(unittest.TestCase):
    def samples(self):
        return [dict(corpus=corpus, permits=permits, read_ms=read_ms, output_ms=output_ms,
                     mode=mode, phase=phase, repetition=0, seconds=0.1, correctness="passed",
                     payload_reads=24 if corpus == "small" else 19,
                     objects=24 if corpus == "small" else 19, peak_readers=1, pack_blake3=corpus, output_writes=30)
                for corpus, permits, read_ms, output_ms, mode, phase in itertools.product(
                    ("small", "boundary"), (1, 2, 8), (0, 5), (0, 2), MODES, ("warmup", "measured"))]

    def test_missing_duplicate_and_corrupt_cases_fail(self):
        samples = self.samples()
        check_samples(samples, 1)
        for bad in (samples[:-1], samples + samples[-1:]):
            with self.assertRaises(AssertionError):
                check_samples(bad, 1)
        for key, value in (("pack_blake3", "wrong"), ("payload_reads", 0), ("peak_readers", 9), ("correctness", "failed"), ("output_writes", 31)):
            with self.subTest(key=key):
                bad = copy.deepcopy(samples)
                bad[0][key] = value
                with self.assertRaises(AssertionError):
                    check_samples(bad, 1)

    def test_all_passes_binary_and_repetitions(self):
        from pathlib import Path
        args = runner.suite_arguments("git-pack-delayed", Path("/tmp/binaries"), "smoke", 6)
        self.assertIn("/tmp/binaries/casita-lib-test", args)
        self.assertEqual(args[args.index("--repetitions") + 1], "6")
