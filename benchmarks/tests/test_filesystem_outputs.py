import json
import unittest

from benchmarks.suites import filesystem_outputs as suite
from benchmarks import all as all_suites
from benchmarks import cli


class FilesystemOutputsTests(unittest.TestCase):
    def sample(self, **changes):
        sample = dict(operation="filesystem-outputs", mode="multi-root", outputs=8,
                      files=1, file_bytes=4096, logical_bytes=32768, nanos=10,
                      session_nanos=1, stage_nanos=2, publish_nanos=3,
                      maintenance_nanos=1, traversal_nanos=1, pages=1, publications=1, walks=1,
                      correctness=suite.CORRECTNESS)
        sample.update(changes)
        return 'filesystem_outputs_sample ' + json.dumps(sample) + '\ntest result: ok. 1 passed; 0 failed;\n'

    def test_exact_sample_and_gates(self):
        self.assertEqual(suite.parse_sample(self.sample(), 8, 1, 4096, "multi-root")["walks"], 1)
        for changes in [dict(correctness=""), dict(files=2), dict(nanos=-1),
                        dict(publications=0), dict(walks=8), dict(logical_bytes=1)]:
            with self.subTest(changes=changes), self.assertRaises(RuntimeError):
                suite.parse_sample(self.sample(**changes), 8, 1, 4096, "multi-root")

    def test_missing_or_duplicate_sample_fails(self):
        for output in ["", self.sample() * 2, self.sample().replace('1 passed', '0 passed')]:
            with self.assertRaises(RuntimeError):
                suite.parse_sample(output, 8, 1, 4096, "multi-root")

    def test_registered_for_all(self):
        self.assertIn("filesystem-outputs", {entry["id"] for entry in cli.entrypoints()})
        import pathlib
        args = all_suites.suite_arguments("filesystem-outputs", pathlib.Path("/tmp/bin"), "smoke", 1)
        self.assertIn("/tmp/bin/casita-lib-test", args)
        self.assertIn("--no-build", args)

    def test_dashboard_keeps_modes_and_scales_separate(self):
        from benchmarks import dashboard
        from pathlib import Path
        samples = [suite.parse_sample(self.sample(), 8, 1, 4096, "multi-root")]
        samples += [{**samples[0], "mode": "per-output"}, {**samples[0], "outputs": 32}]
        samples = [{**sample, "status": "ok", "wall_seconds": 0.1} for sample in samples]
        import tempfile
        result = {"complete": True, "samples": samples, "result_schema": "casita.filesystem-outputs.v1"}
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "result.json"
            path.write_text(json.dumps(result))
            normalized = dashboard.normalize_result(path)
            observations = normalized["observations"]
            self.assertEqual(len(observations), 3)
            self.assertEqual(len({(o["workload"], o["implementation"]) for o in observations}), 3)
            result["complete"] = False
            path.write_text(json.dumps(result))
            with self.assertRaises(ValueError):
                dashboard.normalize_result(path)
