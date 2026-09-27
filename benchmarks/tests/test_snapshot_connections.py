import json
import pathlib
import unittest

from benchmarks import all as all_suites, cli
from benchmarks.suites import snapshot_connections as suite
from benchmarks.suites import repository as common


class SnapshotConnectionTests(unittest.TestCase):
    def test_parser_requires_matching_case_and_correctness(self):
        case = dict(width=9, iterations=8, mode="reused", idle_limit=8,
                    wall_nanos=100, correctness=suite.CORRECTNESS)

        def parse(value, trailer="test result: ok. 1 passed; 0 failed;"):
            return suite.parse_sample("snapshot_connection_sample " + json.dumps(value)
                                      + "\n" + trailer, 9, 8, "reused")

        self.assertEqual(parse(case), case)
        for key, value in (("width", 8), ("iterations", 0), ("mode", "fresh"),
                           ("idle_limit", 9), ("wall_nanos", -1), ("wall_nanos", True),
                           ("correctness", "")):
            with self.subTest(key=key), self.assertRaises(common.BenchmarkError):
                parse({**case, key: value})
        with self.assertRaises(common.BenchmarkError):
            parse(case, "test result: ok. 0 passed; 0 failed;")

    def test_permanent_all_registration(self):
        entry = next(entry for entry in cli.entrypoints() if entry["id"] == "snapshot-connections")
        self.assertEqual(entry["target"], "benchmarks.suites.snapshot_connections")
        args = all_suites.suite_arguments("snapshot-connections", pathlib.Path("/binaries"), "smoke", 1)
        self.assertIn("/binaries/casita-lib-test", args)
        self.assertIn("--no-build", args)
