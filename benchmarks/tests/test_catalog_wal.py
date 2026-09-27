import json
import pathlib
import unittest

from benchmarks.all import suite_arguments
from benchmarks.suites.catalog_wal import CORRECTNESS, parse_sample
from benchmarks.suites.repository import BenchmarkError


class CatalogWalTest(unittest.TestCase):
    def output(self, **changes):
        sample = dict(case="base-below", mode="reference", held=True, iterations=2,
                      correctness=CORRECTNESS, after_release_bytes=0, catalog_bytes=4194300,
                      stored_bytes=48, nanos=100, before_checkpoint_bytes=8272,
                      database_bytes=40960, wal_sizes=[4152, 8272], checkpoint=[0, 2, 0])
        sample.update(changes)
        return "catalog_wal_sample " + json.dumps(sample) + "\ntest result: ok. 1 passed; 0 failed;"

    def test_correctness_and_configuration_required(self):
        for changes in ({"correctness": None}, {"held": False}, {"after_release_bytes": 4096},
                        {"stored_bytes": 1024}, {"wal_sizes": [4152]}, {"checkpoint": []}):
            with self.subTest(changes=changes), self.assertRaises(BenchmarkError):
                parse_sample(self.output(**changes), "base-below", "reference", True, 2)
        self.assertEqual(parse_sample(self.output(), "base-below", "reference", True, 2)["stored_bytes"], 48)

    def test_failed_rust_probe_rejected(self):
        with self.assertRaises(BenchmarkError):
            parse_sample(self.output().replace("ok. 1 passed", "FAILED. 0 passed"),
                         "base-below", "reference", True, 2)

    def test_busy_checkpoint_preserves_unknown_counts(self):
        sample = parse_sample(self.output(checkpoint=[1, None, None]),
                              "base-below", "reference", True, 2)
        self.assertEqual(sample["checkpoint"], [1, None, None])
        with self.assertRaises(BenchmarkError):
            parse_sample(self.output(checkpoint=[0, None, None]),
                         "base-below", "reference", True, 2)

    def test_all_dispatches_frozen_binary(self):
        args = suite_arguments("catalog-wal", pathlib.Path("/bench/bin"), "smoke", 1)
        self.assertIn("/bench/bin/casita-lib-test", args)
        self.assertIn("--no-build", args)

    def test_external_mode_requires_real_descriptor_size(self):
        self.assertEqual(parse_sample(self.output(mode="external", stored_bytes=56),
                                      "base-below", "external", True, 2)["stored_bytes"], 56)
        with self.assertRaises(BenchmarkError):
            parse_sample(self.output(mode="external", stored_bytes=48),
                         "base-below", "external", True, 2)
