import copy
import json
import pathlib
import tempfile
import unittest
from unittest import mock

from benchmarks import dashboard
from benchmarks.suites import metadata_primitives as suite
from benchmarks.suites import repository as common


class PrimitiveTests(unittest.TestCase):
    def test_matrix_retains_operation_timings_and_failed_process_receipts(self):
        for fail in (False, True):
            with self.subTest(fail=fail), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                binary = root / "probe"
                binary.write_bytes(b"fixture")
                output = root / "result.json"
                calls = 0

                def measured(command, stdout, stderr, **kwargs):
                    nonlocal calls
                    calls += 1
                    case = self.case()
                    case.update(count=int(command.env["CASITA_PRIMITIVE_ENTRIES"]),
                                batch=int(command.env["CASITA_PRIMITIVE_BATCH"]))
                    failed = fail and calls == 2
                    stdout.write_text("primitive_sample " + json.dumps(case) +
                                      "\ntest result: ok. 1 passed; 0 failed;\n")
                    stderr.write_text("fixture failure" if failed else "")
                    return {"exit_code": 7 if failed else 0, "wall_seconds": 99, "max_rss_bytes": 1234}

                with mock.patch.object(common, "measured_command", side_effect=measured), \
                     mock.patch.object(common, "environment_metadata", return_value={}):
                    arguments = ["--profile", "smoke", "--repetitions", "1", "--probe-binary", str(binary),
                                 "--no-build", "--output", str(output)]
                    if fail:
                        with self.assertRaisesRegex(common.BenchmarkError, "primitive probe failed"):
                            suite.main(arguments)
                    else:
                        self.assertEqual(suite.main(arguments), 0)
                raw = json.loads(output.read_text())
                self.assertEqual(raw["complete"], not fail)
                self.assertEqual(len(raw["processes"]), 2 if fail else 4)
                self.assertEqual(len(raw["samples"]), len(suite.OPERATIONS) * (1 if fail else 4))
                self.assertEqual(raw["samples"][0]["wall_seconds"], 100.5 / 1e9)
                self.assertEqual(len(raw["samples"][0]["timings"]), 2)
                if fail:
                    self.assertEqual(raw["processes"][-1]["exit_code"], 7)
                    with self.assertRaises(ValueError):
                        dashboard.normalize_result(output)
                else:
                    observations = dashboard.normalize_result(output)["observations"]
                    self.assertEqual(len(observations), 4 * len(suite.OPERATIONS))
                    self.assertEqual({(o["scale"]["entries"], o["scale"]["batch"]) for o in observations},
                                     {(n, b) for n in (256, 257) for b in (1, 16)})

    def case(self):
        return {"count": 257, "batch": 16, "iterations": 2, "correctness": suite.CORRECTNESS,
                "samples": [{"operation": op, "iteration": i, "nanos": 100 + i}
                            for op in suite.OPERATIONS for i in range(2)]}

    def parse(self, case, status="test result: ok. 1 passed; 0 failed;"):
        return suite.parse_sample("primitive_sample " + json.dumps(case) + "\n" + status, 257, 16, 2)

    def test_complete_case(self):
        self.assertEqual(self.parse(self.case()), self.case())

    def test_rejects_wrong_config_missing_gates_and_bad_measurements(self):
        case = self.case()
        variants = []
        for key, value in (("count", 256), ("batch", 1), ("iterations", 1), ("correctness", "")):
            variants.append({**case, key: value})
        variants.extend([{**case, "samples": case["samples"][:-1]},
                         {**case, "samples": case["samples"] + [case["samples"][0]]}])
        for key, value in (("operation", "scan-prefix"), ("nanos", -1), ("nanos", True), ("iteration", False)):
            variant = copy.deepcopy(case)
            variant["samples"][0][key] = value
            variants.append(variant)
        for variant in variants:
            with self.subTest(variant=variant), self.assertRaises(common.BenchmarkError):
                self.parse(variant)
        with self.assertRaises(common.BenchmarkError):
            self.parse(case, "test result: FAILED")
        with self.assertRaises(common.BenchmarkError):
            suite.parse_sample('primitive_sample {\n', 257, 16, 2)

    def test_dashboard_keeps_batch_axis_and_rejects_incomplete_runs(self):
        result = {"schema_version": 1, "result_schema": "casita.metadata-primitives.v1",
                  "suite_id": "state-and-publication", "complete": True,
                  "environment": {}, "configuration": {}, "samples": [
                      {"status": "ok", "implementation": "casita", "operation": "get-batch-hit",
                       "entries": 257, "batch": batch, "wall_seconds": 0.01,
                       "repetition": 1} for batch in (1, 16)]}
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "result.json"
            path.write_text(json.dumps(result))
            normalized = dashboard.normalize_result(path)
            self.assertEqual({o["scale"]["batch"] for o in normalized["observations"]}, {1, 16})
            result["complete"] = False
            path.write_text(json.dumps(result))
            with self.assertRaises(ValueError):
                dashboard.normalize_result(path)


if __name__ == "__main__":
    unittest.main()
