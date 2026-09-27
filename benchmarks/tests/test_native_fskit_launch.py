from pathlib import Path
import json
import ctypes
import struct
import tempfile
import unittest
from unittest import mock

from benchmarks.suites import native_fskit_launch as suite
from benchmarks.suites import native_fskit_launch_density as density
from benchmarks.suites.repository import BenchmarkError


class LaunchTests(unittest.TestCase):
    def test_timed_listing_rejects_duplicates_and_missing_names(self):
        for names in (["a", "b"], ["a", "a", "b"], ["a"], ["a", "c"]):
            with mock.patch.object(suite.os, "listdir", return_value=names):
                if names == ["a", "b"]:
                    self.assertEqual(suite.launch(Path("/fixture"), "native-listdir", ["a", "b"])["entries"], 2)
                else:
                    with self.assertRaisesRegex(BenchmarkError, "listing differs"):
                        suite.launch(Path("/fixture"), "native-listdir", ["a", "b"])
        with self.assertRaisesRegex(BenchmarkError, "independent names oracle"):
            suite.launch(Path("/fixture"), "native-listdir")

    def test_bundle_control_releases_owned_objects_on_failure(self):
        cf = mock.Mock()
        cf.CFURLCreateFromFileSystemRepresentation.return_value = 1
        cf.CFBundleCreate.return_value = 2
        cf.CFBundleCopyExecutableURL.return_value = 3
        with mock.patch.object(suite, "security_functions", return_value=(cf, None)):
            with self.assertRaisesRegex(BenchmarkError, "unexpectedly identified"):
                suite.bundle_discovery_query(Path("/fixture"))
        self.assertEqual(cf.CFRelease.call_args_list, [mock.call(3), mock.call(2), mock.call(1)])
        cf.reset_mock()
        cf.CFBundleCreate.return_value = None
        with mock.patch.object(suite, "security_functions", return_value=(cf, None)):
            with self.assertRaisesRegex(BenchmarkError, "CFBundleCreate failed"):
                suite.bundle_discovery_query(Path("/fixture"))
        cf.CFRelease.assert_called_once_with(1)

    def test_volume_capabilities_reject_failed_or_malformed_reply(self):
        library = mock.Mock()
        with mock.patch.object(suite, "path_functions", return_value=library):
            library.getattrlist.return_value = -1
            with self.assertRaisesRegex(BenchmarkError, "capabilities query failed"):
                suite.volume_capabilities(Path("/fixture"))
            library.getattrlist.return_value = 0
            with self.assertRaisesRegex(BenchmarkError, "capabilities length"):
                suite.volume_capabilities(Path("/fixture"))
            def respond(_path, _request, buffer, _size, _options):
                buffer.raw = struct.pack("=9I", 36, 0x20720, 0, 0, 0, 0x20721, 0, 0, 0)
                return 0
            library.getattrlist.side_effect = respond
            self.assertEqual(suite.volume_capabilities(Path("/fixture")),
                             {"capabilities":[0x20720, 0, 0, 0], "valid":[0x20721, 0, 0, 0]})

    def test_static_code_failure_releases_url(self):
        cf, security = mock.Mock(), mock.Mock()
        cf.CFURLCreateFromFileSystemRepresentation.return_value = 123
        security.SecStaticCodeCreateWithPath.return_value = -1
        with mock.patch.object(suite, "security_functions", return_value=(cf, security)):
            with self.assertRaisesRegex(BenchmarkError, "static-code creation failed"):
                suite.static_code_query(Path("/fixture"))
        cf.CFRelease.assert_called_once_with(123)

    def test_attribute_string_checks_bounds_and_termination(self):
        good = struct.pack("=IiI", 17, 8, 5) + b"name\0"
        self.assertEqual(suite.attribute_string(ctypes.create_string_buffer(good)), b"name")
        for bad in (struct.pack("=IiI", 17, -4, 5) + b"name\0",
                    struct.pack("=IiI", 17, 8, 20) + b"name\0",
                    struct.pack("=IiI", 17, 8, 5) + b"names"):
            with self.assertRaises(BenchmarkError):
                suite.attribute_string(ctypes.create_string_buffer(bad))

    def test_xattr_controls_reject_data_or_mutation(self):
        library = mock.Mock()
        with mock.patch.object(suite, "xattr_functions", return_value=library):
            library.getxattr.return_value = 1
            with self.assertRaises(BenchmarkError):
                suite.xattr_query(Path("/fixture"), "native-getxattr")
            library.listxattr.return_value = 1
            with self.assertRaises(BenchmarkError):
                suite.xattr_query(Path("/fixture"), "native-listxattr")
            library.setxattr.return_value = 0
            with self.assertRaises(BenchmarkError):
                suite.xattr_mutation_gate(Path("/fixture"))

    def test_density_reuses_build_and_rejects_mismatch(self):
        for mismatch in (False, True):
            with tempfile.TemporaryDirectory() as directory:
                output = Path(directory) / "density.json"
                calls = []
                def run(arguments):
                    calls.append(arguments)
                    path = Path(arguments[arguments.index("--output")+1])
                    path.write_text(json.dumps(dict(complete=True, comparison_complete=True,
                        binaries={"extension":"changed" if mismatch and len(calls)>1 else "same"},
                        server_sha256="same", build_identity={}, source_sha256={},
                        bundle="/bundle", server_binary="/server")))
                    return 0
                with mock.patch.object(density, "COUNTS", (0, 128)), \
                     mock.patch.object(density.launch, "main", side_effect=run):
                    code = density.main(["--output", str(output), "--enumeration-cache", "disabled", "--enumeration-timing", "detailed", "--filename-construction", "bytes", "--volume-capabilities", "explicit", "--item-timestamps", "store", "--reader-cache", "disabled"])
                self.assertEqual(code, int(mismatch))
                self.assertEqual(json.loads(output.read_text())["complete"], not mismatch)
                self.assertIn("--bundle", calls[1])
                self.assertIn("--server-binary", calls[1])
                for arguments in calls:
                    self.assertEqual(arguments[arguments.index("--enumeration-cache") + 1], "disabled")
                    self.assertEqual(arguments[arguments.index("--enumeration-timing") + 1], "detailed")
                    self.assertEqual(arguments[arguments.index("--filename-construction") + 1], "bytes")
                    self.assertEqual(arguments[arguments.index("--volume-capabilities") + 1], "explicit")
                    self.assertEqual(arguments[arguments.index("--item-timestamps") + 1], "store")
                    self.assertEqual(arguments[arguments.index("--reader-cache") + 1], "disabled")

    def test_getpath_control_rejects_incorrect_path(self):
        with tempfile.TemporaryDirectory() as directory:
            tree = Path(directory)
            (tree / "native-executable").write_bytes(b"fixture")
            with mock.patch.object(suite.fcntl, "fcntl", return_value=b"/wrong/path\0"):
                with self.assertRaisesRegex(BenchmarkError, "wrong path"):
                    suite.launch(tree, "native-fgetpath")
            expected = str((tree / "native-executable").resolve()).encode()+b"\0"
            with mock.patch.object(suite.fcntl, "fcntl", return_value=expected):
                self.assertGreater(suite.launch(tree, "native-fgetpath")["getpath_ns"], 0)

    def test_launch_checks_exit_and_output_in_both_wait_modes(self):
        with tempfile.TemporaryDirectory() as directory:
            tree = Path(directory)
            script = tree / "run"
            script.write_text("#!/bin/sh\nprintf 'casita-native-fskit-ok\\n'\n")
            script.chmod(0o755)
            for case in ("script-timeout", "script-blocking", "script-interpreted-blocking"):
                result = suite.launch(tree, case)
                self.assertEqual(result["total_ns"], result["spawn_ns"] + result["communicate_ns"])
                script.write_text("#!/bin/sh\nprintf wrong\n")
                with self.assertRaises(BenchmarkError):
                    suite.launch(tree, case)
                script.write_text("#!/bin/sh\nprintf 'casita-native-fskit-ok\\n'\nexit 1\n")
                with self.assertRaises(BenchmarkError):
                    suite.launch(tree, case)
                script.write_text("#!/bin/sh\nprintf 'casita-native-fskit-ok\\n'\n")

    def test_comparison_requires_host_and_matching_successful_samples(self):
        rows = [dict(case=case, repetition=0, implementation=name, correctness="passed",
                     elapsed_ns=[10, 20], p50_ns=20)
                for case in suite.CASES for name in ("native", "host")]
        self.assertEqual(len(suite.comparisons(rows, 1, "native")), len(suite.CASES))
        for invalid in (rows[:-1], rows + [rows[0]],
                        [{**rows[0], "correctness":"failed"}, *rows[1:]],
                        [{**rows[0], "elapsed_ns":[10]}, *rows[1:]]):
            with self.assertRaises(BenchmarkError):
                suite.comparisons(invalid, 1, "native")


if __name__ == "__main__":
    unittest.main()
