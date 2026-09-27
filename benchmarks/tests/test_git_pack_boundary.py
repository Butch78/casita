import copy
import itertools
import unittest
from pathlib import Path
from benchmarks.suites.git_pack_boundary import CORPORA, MODES, validate
from benchmarks import all as runner


class BoundaryTests(unittest.TestCase):
    def test_validation_requires_every_control_and_order(self):
        rows = [dict(corpus=corpus, compression_level=level, mode=mode, repetition=0,
                     phase=phase, seconds=0.1, correctness="passed", read_ms=0, output_ms=0,
                     permits=1, peak_readers=1, objects=count, payload_reads=count,
                     pack_blake3=f"{corpus}-{level}", output_writes=4)
                for (corpus, count), level, phase, mode in itertools.product(
                    CORPORA.items(), (0, 6), ("warmup", "measured"), MODES)]
        self.assertEqual(len(validate(rows, 1)), 12)
        for bad in (rows[:-1], rows + rows[-1:], list(reversed(rows))):
            with self.assertRaises(AssertionError): validate(bad, 1)
        for key, value in (("compression_level", 5), ("read_ms", 1), ("output_writes", 5),
                           ("pack_blake3", "wrong"), ("payload_reads", 0)):
            bad = copy.deepcopy(rows)
            bad[0][key] = value
            with self.subTest(key=key), self.assertRaises(AssertionError): validate(bad, 1)

    def test_all_registers_binary(self):
        args = runner.suite_arguments("git-pack-boundary", Path("/tmp/bin"), "standard", 30)
        self.assertIn("/tmp/bin/casita-lib-test", args)
        self.assertEqual(args[args.index("--repetitions") + 1], "30")
