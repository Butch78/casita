import copy
import json
import pathlib
import tempfile
import unittest
from unittest import mock

from benchmarks import all as all_suites, cli, dashboard, revisions
from benchmarks.suites import metadata_collection as collection


class MetadataCollectionTests(unittest.TestCase):
    def case(self, count=65537, iterations=2):
        return dict(count=count, iterations=iterations, correctness=collection.CORRECTNESS,
                    samples=[dict(iteration=i, warm=i > 0, nanos=(i + 1) * 1000) for i in range(iterations + 1)])

    def output(self, cases):
        return "\n".join("collection_sample " + json.dumps(case) for case in cases) + "\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n"

    def test_probe_requires_exact_case_and_all_ordered_iterations(self):
        case = self.case()
        self.assertEqual(collection.parse_sample(self.output([case]), 65537, 2), case)
        broken = [[], [case, case], [self.case(65536)], [self.case(iterations=1)]]
        for field, value in [("correctness", None), ("samples", case["samples"][:-1]),
                             ("samples", list(reversed(case["samples"])))]:
            broken.append([{**case, field: value}])
        for cases in broken:
            with self.subTest(cases=cases), self.assertRaises(collection.common.BenchmarkError):
                collection.parse_sample(self.output(cases), 65537, 2)
        with self.assertRaises(collection.common.BenchmarkError):
            collection.parse_sample(self.output([case]).replace("1 passed", "0 passed"), 65537, 2)

    def test_probe_rejects_invalid_timings_and_phase_labels(self):
        for field, values in [("nanos", [-1, 0.5, True, None]), ("iteration", [True, 9]), ("warm", [False, 1])]:
            for value in values:
                case = copy.deepcopy(self.case())
                case["samples"][1][field] = value
                with self.subTest(field=field, value=value), self.assertRaises(collection.common.BenchmarkError):
                    collection.parse_sample(self.output([case]), 65537, 2)

    def run_fixture(self, root, *, fail=False, probe="collection"):
        binary = root / "probe"
        binary.write_bytes(b"test probe")
        output = root / "result.json"

        def measured(command, stdout, stderr, **kwargs):
            env = command.env
            case = self.case(int(env["CASITA_COLLECTION_COUNTS"]), int(env["CASITA_COLLECTION_ITERATIONS"]))
            if probe == "ordered-scan":
                self.assertIn(collection.SCAN_PROBE, command.steps[0])
                case["correctness"] = collection.SCAN_CORRECTNESS
            captured = self.output([case])
            stdout.write_text(captured.replace("collection_sample ", "scan_sample ") if probe == "ordered-scan" else captured)
            stderr.write_text("fixture failure" if fail else "")
            return dict(exit_code=7 if fail else 0, wall_seconds=99, max_rss_bytes=1234)

        with mock.patch.object(collection.common, "measured_command", side_effect=measured), mock.patch.object(collection.common, "environment_metadata", return_value={}):
            args = ["--probe", probe, "--profile", "smoke", "--repetitions", "1", "--probe-binary", str(binary), "--no-build", "--output", str(output)]
            if fail:
                with self.assertRaisesRegex(collection.common.BenchmarkError, "probe failed"):
                    collection.main(args)
            else:
                self.assertEqual(collection.main(args), 0)
        return output

    def test_smoke_runs_both_sides_and_normalizes_separate_phases_and_sizes(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = self.run_fixture(pathlib.Path(temporary))
            raw = json.loads(output.read_text())
            self.assertTrue(raw["complete"])
            self.assertEqual(raw["configuration"]["counts"], [65536, 65537])
            self.assertEqual(len(raw["processes"]), 2)
            self.assertEqual(len(raw["artifacts"][0]["sha256"]), 64)
            normalized = dashboard.normalize_result(output)
            observations = normalized["observations"]
            self.assertEqual({(o["scale"]["entries"], o["cache_policy"]) for o in observations},
                             {(n, phase) for n in (65536, 65537) for phase in ("first", "warm")})
            for observation in observations:
                expected = 1e-6 if observation["cache_policy"] == "first" else 2e-6
                self.assertEqual(observation["metrics"]["wall_seconds"], expected)
                self.assertEqual(observation["metrics"]["max_rss_bytes"], 1234)
            raw["complete"] = False
            output.write_text(json.dumps(raw))
            with self.assertRaisesRegex(ValueError, "incomplete"):
                dashboard.normalize_result(output)

    def test_failed_process_preserves_receipt_and_cannot_be_compared(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = self.run_fixture(pathlib.Path(temporary), fail=True)
            raw = json.loads(output.read_text())
            self.assertFalse(raw["complete"])
            self.assertEqual(raw["samples"], [])
            self.assertEqual(raw["processes"][0]["exit_code"], 7)
            self.assertIn("fixture failure", raw["error"])
            with self.assertRaisesRegex(ValueError, "incomplete"):
                dashboard.normalize_result(output)

    def test_scan_smoke_covers_page_boundary_and_keeps_correct_order_gate(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = self.run_fixture(pathlib.Path(temporary), probe="ordered-scan")
            raw = json.loads(output.read_text())
            self.assertTrue(raw["complete"])
            self.assertEqual(raw["configuration"]["counts"], [256, 257, 8192])
            self.assertTrue(all("scans" in sample and "commits" not in sample for sample in raw["samples"]))
            normalized = dashboard.normalize_result(output)
            self.assertEqual(normalized["suite_id"], "state-and-publication")
            self.assertEqual({(o["scale"]["entries"], o["cache_policy"]) for o in normalized["observations"]},
                             {(count, phase) for count in (256, 257, 8192) for phase in ("first", "warm")})
            wrong_gate = self.output([self.case()]).replace("collection_sample ", "scan_sample ")
            with self.assertRaises(collection.common.BenchmarkError):
                collection.parse_sample(wrong_gate, 65537, 2, "ordered-scan")
        entry = next(e for e in cli.entrypoints() if e["id"] == "metadata-scan")
        args = collection.build_parser().parse_args(entry["default_arguments"] + ["--output", "/result.json"])
        self.assertEqual(args.probe, "ordered-scan")
        self.assertEqual(revisions.SUITE_BUILD_SPECS[entry["id"]].cargo_arguments, collection.CARGO_ARGUMENTS)

    def test_registered_for_all_suites_and_revision_comparisons(self):
        entry = next(e for e in cli.entrypoints() if e["id"] == "metadata-collection")
        self.assertEqual(entry["suite_id"], "collection-and-fsck")
        args = collection.build_parser().parse_args(all_suites.suite_arguments(
            entry["id"], pathlib.Path("/binaries"), "smoke", 1) + ["--output", "/result.json"])
        self.assertEqual(args.profile, "smoke")
        self.assertEqual(args.probe_binary, pathlib.Path("/binaries/casita-lib-test"))
        self.assertTrue(args.no_build)
        self.assertEqual(revisions.SUITE_BUILD_SPECS[entry["id"]].cargo_arguments, collection.CARGO_ARGUMENTS)
