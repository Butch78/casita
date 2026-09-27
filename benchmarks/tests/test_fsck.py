from __future__ import annotations

import json
import pathlib
import tempfile
import unittest
from unittest import mock

from benchmarks import dashboard
from benchmarks.suites import fsck


def summary(*, objects=321, payloads=321, roots=1, revision="rev-original"):
    return (f"revision {revision}; checked {roots} root(s), {objects} object(s), {payloads} payload(s)\n"
            "traversal-spill-files-opened 2; traversal-spill-peak-bytes 32768\n"
            "logical-fsck-nanos 123456789\n")


class FsckTests(unittest.TestCase):
    def test_memory_limits_cover_sustained_spill_and_cache_boundaries(self):
        parser = fsck.build_parser()
        for profile, count, expected in [
            ("smoke", 321, [32, 128, 320, 321, 322]),
            ("standard", 65601, [1024, 4096, 65600, 65601, 65602]),
        ]:
            args = parser.parse_args(["--profile", profile, "--output", "/result.json"])
            self.assertEqual(fsck.memory_limits(args, count), expected)
        args = parser.parse_args(["--memory-objects", "1024,4096", "--output", "/result.json"])
        limits = fsck.memory_limits(args, 65601)
        self.assertEqual(limits, [1024, 4096])
        limits.reverse()
        self.assertEqual(args.memory_objects, [1024, 4096])

    def test_runner_measures_and_checks_all_default_memory_limits(self):
        with tempfile.TemporaryDirectory() as directory:
            work = pathlib.Path(directory)
            (work / "256").mkdir()
            args = fsck.build_parser().parse_args([
                "--profile", "smoke", "--repetitions", "1", "--reuse-work-dir", str(work),
                "--output", str(work / "result.json"),
            ])
            args.artifacts = [dict(path=v, variant=v, sha256=v) for v in ("baseline", "candidate")]
            args.casita = "candidate"

            def checked(command):
                return "object-key bench/retained\n" if command[-2:] == ["root", "ls"] else summary()

            def measured(command, stdout, stderr, **kwargs):
                invocation = command.steps[0]
                threshold = int(invocation[invocation.index("--spill-memory-objects") + 1])
                output = summary()
                if threshold > 321:
                    output = output.replace("files-opened 2", "files-opened 0")
                stdout.write_text(output)
                stderr.write_text("")
                return dict(exit_code=0, wall_seconds=0.1, max_rss_bytes=1024)

            result = dict(fixtures=[], samples=[])
            with mock.patch.object(fsck, "checked", side_effect=checked), \
                 mock.patch.object(fsck, "audit_checkout"), \
                 mock.patch.object(fsck.common, "tree_manifest", return_value={}), \
                 mock.patch.object(fsck.common, "manifest_identity", return_value="manifest"), \
                 mock.patch.object(fsck.common, "measured_command", side_effect=measured), \
                 mock.patch.object(fsck.graph, "active_spill_files", return_value=[]):
                fsck.run_fixture(args, result, work, 256)
            self.assertEqual(len(result["samples"]), 10)
            self.assertEqual({(s["variant"], s["spill_memory_objects"]) for s in result["samples"]},
                             {(v, n) for v in ("baseline", "candidate") for n in (32, 128, 320, 321, 322)})
            self.assertTrue(all(s["status"] == "ok" for s in result["samples"]))
            self.assertEqual(result["fixtures"][0]["checkout_after"], "ok")

    def test_seed_requires_exact_counts_and_a_passing_native_test(self):
        output = 'fsck_fixture {"files":256,"objects":321}\ntest result: ok. 1 passed; 0 failed;\n'
        self.assertEqual(fsck.parse_seed(output, 256, 321)["objects"], 321)
        for invalid in (output.replace("321", "320"), output.replace("1 passed", "0 passed"),
                        output + 'fsck_fixture {}\n', 'fsck_fixture broken\n',
                        output.replace('{"files":256,"objects":321}', '[]')):
            with self.subTest(output=invalid), self.assertRaises(fsck.common.BenchmarkError):
                fsck.parse_seed(invalid, 256, 321)

    def test_report_requires_exact_inventory_and_revision(self):
        metrics = fsck.parse_fsck(summary(), 321, "rev-original")
        self.assertEqual(metrics["logical_fsck_seconds"], 0.123456789)
        self.assertEqual(metrics["spill_peak_bytes"], 32768)
        for kwargs in ({"objects": 320}, {"payloads": 320}, {"roots": 0}, {"revision": "rev-changed"}):
            with self.subTest(kwargs=kwargs), self.assertRaises(fsck.common.BenchmarkError):
                fsck.parse_fsck(summary(**kwargs), 321, "rev-original")

    def test_rejects_partial_duplicate_and_unexpected_issues(self):
        for output in ("", summary().splitlines()[0], summary() * 2,
                       summary() + "Corrupt MissingRecord object\n",
                       summary().replace("123456789", "-1")):
            with self.subTest(output=output), self.assertRaises(fsck.common.BenchmarkError):
                fsck.parse_fsck(output, 321)

    def test_dashboard_keeps_cache_limits_separate_and_rejects_incomplete(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "result.json"
            result = {"result_schema": "casita.fsck.v1", "suite_id": "collection-and-fsck",
                      "complete": True, "configuration": {"profile": "smoke"},
                      "samples": [{"status": "ok", "operation": "fsck", "entries": 321,
                                   "spill_memory_objects": threshold, "wall_seconds": 0.1,
                                   "max_rss_bytes": 1024} for threshold in (320, 321, 322)]}
            path.write_text(json.dumps(result))
            observations = dashboard.normalize_result(path)["observations"]
            self.assertEqual(len(observations), 3)
            self.assertEqual({row["scale"]["spill_memory_objects"] for row in observations}, {320, 321, 322})
            result["samples"] = [{**sample, "variant": variant}
                                 for sample in result["samples"] for variant in ("baseline", "candidate")]
            path.write_text(json.dumps(result))
            paired = dashboard.normalize_result(path)["observations"]
            self.assertEqual(len(paired), 6)
            self.assertEqual(len({row["workload"] for row in paired}), 3)
            self.assertEqual({row["implementation"] for row in paired}, {"casita-baseline", "casita-candidate"})
            result["complete"] = False
            path.write_text(json.dumps(result))
            with self.assertRaisesRegex(ValueError, "incomplete fsck"):
                dashboard.normalize_result(path)

    def test_failure_preserves_completed_samples_and_excludes_result(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "result.json"
            executable = pathlib.Path(directory) / "casita"
            executable.write_text("binary identity")

            def fail(args, result, work, files):
                result["samples"].append({"status": "ok", "operation": "fsck"})
                raise fsck.common.BenchmarkError("checkout mismatch")

            with mock.patch.object(fsck.shutil, "which", return_value=str(executable)), \
                 mock.patch.object(fsck.common, "environment_metadata", return_value={}), \
                 mock.patch.object(fsck, "run_fixture", side_effect=fail):
                code = fsck.main(["--profile", "smoke", "--output", str(path)])
            result = json.loads(path.read_text())
            self.assertEqual(code, 1)
            self.assertFalse(result["complete"])
            self.assertEqual(result["error"], "checkout mismatch")
            self.assertEqual(len(result["samples"]), 1)

    def test_profiles_include_default_limit_frontier(self):
        files = fsck.PROFILES["frontier"][0]
        branches = max(64, (files + 1023) // 1024)
        self.assertEqual(files + branches + 1, 250000)
        self.assertLessEqual((files + branches - 1) // branches, 1024)

    def test_paired_runs_reject_identical_binaries(self):
        with tempfile.TemporaryDirectory() as directory:
            executable = pathlib.Path(directory) / "casita"
            executable.write_text("same binary")
            with mock.patch.object(fsck.shutil, "which", return_value=str(executable)):
                with self.assertRaises(SystemExit):
                    fsck.main(["--casita", str(executable), "--baseline-casita", str(executable),
                               "--output", str(pathlib.Path(directory) / "result.json")])


if __name__ == "__main__":
    unittest.main()
