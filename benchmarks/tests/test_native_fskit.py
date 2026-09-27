import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

from benchmarks import all as all_suites
from benchmarks.suites import native_fskit as suite
from benchmarks.suites import native_fskit_portable as portable_suite
from benchmarks.suites.repository import BenchmarkError


class NativeFSKitTests(unittest.TestCase):
    def test_portable_fixture_only_changes_the_problem_filename(self):
        with mock.patch.object(portable_suite, "native_main", return_value=0) as run:
            self.assertEqual(portable_suite.main(["--output", "result.json"]), 0)
        run.assert_called_once_with(["--portable-names", "--output", "result.json"])
        raw, portable = suite.fixture(), suite.fixture(portable=True)
        self.assertEqual(raw.pop(suite.os.fsdecode(b"byte-\xff")), portable.pop("byte-ascii"))
        self.assertEqual(raw, portable)
        pairs = suite.paired_comparisons(self.controlled_samples(), 1, suite.PORTABLE_FIXTURE_ID)
        self.assertTrue(all(pair["fixture"] == suite.PORTABLE_FIXTURE_ID for pair in pairs))

    def test_registration_uses_shared_rust_setup_for_each_module(self):
        for repository in (False, True):
            runner = suite.NativeRun(Path("/new"), {"commands": [], "configuration": {"repository": repository}})
            app = Path("/new/CasitaNativeFSKit.app")
            with mock.patch.object(runner, "command", return_value=b"") as command:
                runner.register(app)
            calls = [call.args[0] for call in command.call_args_list]
            self.assertEqual(calls[-1][1:], [app, runner.identifier])
            self.assertEqual(calls[-1][0].name, "fskit-native-setup")
            self.assertIn("--locked", calls[0])

    def test_mount_retries_only_unmounted_helper_startup_failures(self):
        runner = suite.NativeRun(Path("/new"), {"commands": []})
        args = ["/sbin/mount", "-F", "-t", "casitanative", "resource", Path("/mount")]
        def first_failure_then_success(_args):
            if not runner.result["commands"]:
                runner.result["commands"].append({"returncode": 69, "stderr": "com.apple.extensionKit.errorDomain error 2: Unable to invoke task"})
                raise BenchmarkError("helper unavailable")
            return b"mounted"
        with mock.patch.object(runner, "command", side_effect=first_failure_then_success) as command, \
             mock.patch.object(suite.os.path, "ismount", return_value=False), \
             mock.patch.object(suite.time, "monotonic", return_value=0), \
             mock.patch.object(suite.time, "sleep"):
            self.assertEqual(runner.mount_command(args), b"mounted")
            self.assertEqual(command.call_count, 2)

    def test_mount_retry_preserves_fatal_ambiguous_and_expired_failures(self):
        for code, stderr, mounted, now in ((69, "permission denied", False, 0),
                                          (69, "com.apple.extensionKit.errorDomain error 2: Unable to invoke task", True, 0),
                                          (None, "Unable to invoke task", False, 0),
                                          (69, "com.apple.extensionKit.errorDomain error 2: Unable to invoke task", False, 15)):
            with self.subTest(code=code, stderr=stderr, mounted=mounted, now=now):
                runner = suite.NativeRun(Path("/new"), {"commands": [{"returncode": code, "stderr": stderr}]})
                with mock.patch.object(runner, "command", side_effect=BenchmarkError("original failure")) as command, \
                     mock.patch.object(suite.os.path, "ismount", return_value=mounted), \
                     mock.patch.object(suite.time, "monotonic", side_effect=[0, now]), \
                     mock.patch.object(suite.time, "sleep") as sleep:
                    with self.assertRaisesRegex(BenchmarkError, "original failure"):
                        runner.mount_command(["/sbin/mount", Path("/mount")])
                    command.assert_called_once()
                    sleep.assert_not_called()

    def test_explicit_sdk_is_identified_without_xcode_select(self):
        with tempfile.TemporaryDirectory() as directory:
            sdk = Path(directory)
            (sdk / "SDKSettings.json").write_text('{"Version":"26.4"}')
            runner = suite.NativeRun(sdk, {"commands": []})
            with mock.patch.dict(suite.os.environ, {"SDKROOT": str(sdk)}), \
                 mock.patch.object(runner, "command", return_value=b"tool version") as command:
                identity = runner.build_identity()
            self.assertEqual(identity["sdk"]["version"], "26.4")
            self.assertEqual(identity["sdk"]["root"], str(sdk.resolve()))
            self.assertTrue(all(call.args[0][0] != "xcrun" for call in command.call_args_list))

    def controlled_samples(self):
        dimensions = [("readdir", None, None), ("stat-256", None, None)]
        dimensions += [(case, size, None) for size in suite.SIZES for case in ("open-read-close", "held-fd-read")]
        dimensions += [("parallel-read-32", None, workers) for workers in (1, 4, 16)]
        return [dict(case=case, size=size, workers=workers, repetition=0, implementation=implementation,
                     p50_ns=nanos, p95_ns=nanos, elapsed_ns=[nanos] * 3, correctness="passed")
                for case, size, workers in dimensions
                for implementation, nanos in ((suite.NATIVE, 10), (suite.HOST, 20))]

    def test_controlled_pairing_never_uses_production_storage_as_memory_baseline(self):
        samples = self.controlled_samples()
        production = [{**row, "implementation": "unrelated-backend", "p50_ns": 9000}
                      for row in samples if row["implementation"] == suite.HOST]
        pairs = suite.paired_comparisons(samples + production, 1)
        self.assertEqual(len(pairs), 39)
        self.assertTrue(all(row["p50_host_over_native"] == 2 for row in pairs))
        native_only = [row for row in samples if row["implementation"] == suite.NATIVE]
        with self.assertRaises(BenchmarkError):
            suite.paired_comparisons(native_only + production, 1)

    def test_missing_duplicate_failed_and_mismatched_samples_are_rejected(self):
        samples = self.controlled_samples()
        with self.assertRaises(BenchmarkError):
            suite.paired_comparisons(samples, 1, extra_dimensions=[("execute-native", None, None)])
        for invalid in (samples[:-1], samples + [samples[0]],
                        [{**samples[0], "correctness": "failed"}, *samples[1:]],
                        [{**samples[0], "elapsed_ns": [10]}, *samples[1:]]):
            with self.subTest(invalid=invalid[0]), self.assertRaises(BenchmarkError):
                suite.paired_comparisons(invalid, 1)

    def test_corrupt_or_missing_content_fails_correctness_gate(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "fixture"
            files = suite.fixture(portable=True)
            suite.materialize(root, files)
            suite.check_tree(root, files)
            target = root / "size-4097"
            target.chmod(0o644)
            target.write_bytes(b"wrong")
            with self.assertRaises(BenchmarkError):
                suite.check_tree(root, files)
            target.unlink()
            with self.assertRaises(BenchmarkError):
                suite.check_tree(root, files)

    def test_failed_unmount_is_reported_and_never_forced(self):
        with tempfile.TemporaryDirectory() as directory:
            result = {"commands": []}
            runner = suite.NativeRun(Path(directory), result)
            root = Path(directory) / "mount"
            runner.mounts = [root]
            runner.devices = ["/dev/disk-fixture"]
            with mock.patch.object(suite.os.path, "ismount", return_value=True), \
                 mock.patch.object(runner, "command", side_effect=BenchmarkError("busy")) as command:
                self.assertFalse(runner.cleanup())
                self.assertEqual(len(result["cleanup_errors"]), 2)
                self.assertEqual(runner.mounts, [root])
                self.assertTrue(all("-force" not in call.args[0] for call in command.call_args_list))

    def test_all_on_linux_records_native_suite_as_skipped(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "all"
            with mock.patch.object(all_suites.sys, "platform", "linux"), \
                 mock.patch.object(all_suites.common, "environment_metadata", return_value={}):
                code = all_suites.main(["--suites", "native-fskit,native-fskit-portable,native-fskit-repository,native-fskit-launch,native-fskit-launch-uncached,native-fskit-launch-eager,native-fskit-launch-enumeration-uncached,native-fskit-launch-density-enumeration-uncached,native-fskit-launch-density,native-fskit-launch-density-phases,native-fskit-launch-capabilities,native-fskit-launch-zero-times,native-fskit-first-launch,native-fskit-workloads,native-fskit-workloads-readers-16,native-fskit-workloads-uncached,native-fskit-workloads-read-trace,native-fskit-first-launch-uncached,native-fskit-launch-density-filename-bytes,native-fskit-launch-profile,native-fskit-launch-explicit-xattrs,native-fskit-launch-density-explicit-xattrs", "--output", str(output)])
            ledger = json.loads((output / "execution.json").read_text())
            self.assertEqual(code, 1)
            self.assertFalse(ledger["complete"])
            self.assertEqual(ledger["entries"][0]["status"], "skipped")
            self.assertEqual(ledger["entries"][1]["status"], "skipped")
            self.assertEqual(ledger["entries"][2]["status"], "skipped")
            self.assertEqual(ledger["entries"][3]["status"], "skipped")
            self.assertEqual(ledger["entries"][4]["status"], "skipped")
            self.assertEqual(ledger["entries"][5]["status"], "skipped")
            self.assertEqual(ledger["entries"][6]["status"], "skipped")
            self.assertEqual(ledger["entries"][7]["status"], "skipped")
            self.assertEqual(ledger["entries"][8]["status"], "skipped")
            self.assertEqual(len(ledger["entries"]), 22)
            self.assertEqual(ledger["entries"][9]["status"], "skipped")
            self.assertEqual(ledger["entries"][10]["status"], "skipped")
            self.assertEqual(ledger["entries"][11]["status"], "skipped")
            self.assertEqual(ledger["entries"][12]["status"], "skipped")
            self.assertEqual(ledger["entries"][13]["status"], "skipped")
            self.assertEqual(ledger["entries"][14]["status"], "skipped")


if __name__ == "__main__":
    unittest.main()
