import json
import pathlib
import unittest

from benchmarks import all as runner, cli
from benchmarks.suites import ingest_scheduling as suite
from benchmarks.suites import repository as common


class IngestSchedulingTests(unittest.TestCase):
    def test_only_matching_successful_bounded_cases_are_accepted(self):
        case = dict(files=17, concurrency=16, pattern="skewed", mode="ready", page_limit=1024,
                    peak_active=16, largest_page=18, wall_nanos=100, correctness=suite.CORRECTNESS)

        def parse(value, trailer="test result: ok. 1 passed; 0 failed;"):
            return suite.parse_sample("ingest_scheduling_sample " + json.dumps(value) + "\n" + trailer,
                                      17, 16, "skewed", "ready")

        self.assertEqual(parse(case), case)
        for key, value in (("files", 16), ("concurrency", 1), ("mode", "ordered"),
                           ("pattern", "uniform"), ("page_limit", 2048), ("peak_active", 17),
                           ("largest_page", 1025), ("wall_nanos", True), ("wall_nanos", 0),
                           ("correctness", "")):
            with self.subTest(key=key), self.assertRaises(common.BenchmarkError):
                parse({**case, key: value})
        with self.assertRaises(common.BenchmarkError):
            parse(case, "test result: ok. 0 passed; 0 failed;")

    def test_permanent_registration_supplies_the_probe_binary(self):
        entry = next(entry for entry in cli.entrypoints() if entry["id"] == "ingest-scheduling")
        self.assertEqual(entry["target"], "benchmarks.suites.ingest_scheduling")
        args = runner.suite_arguments(entry["id"], pathlib.Path("/binaries"), "smoke", 1)
        self.assertIn("/binaries/casita-lib-test", args)
        self.assertIn("--no-build", args)
