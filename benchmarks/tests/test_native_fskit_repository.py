import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

from benchmarks.suites import native_fskit_repository as suite
from benchmarks.suites.repository import BenchmarkError


class RepositoryFSKitTests(unittest.TestCase):
    def test_extracted_callback_changes_invalidate_bundle_identity(self):
        identity = suite.source_identity()
        key = "crates/fskit-native/src/native/volume.rs"
        self.assertIn(key, identity)
        original = Path.read_bytes
        def changed(path):
            data = original(path)
            return data + b"\n// changed callback\n" if path == suite.cli.ROOT / key else data
        with mock.patch.object(Path, "read_bytes", changed):
            updated = suite.source_identity()
        self.assertNotEqual(identity[key], updated[key])
        self.assertEqual(set(identity), set(updated))

    def test_correctness_rejects_nested_corruption_and_missing_names(self):
        with tempfile.TemporaryDirectory() as directory:
            source, tree = Path(directory) / "source", Path(directory) / "tree"
            files = suite.fixture(source, "test")
            suite.shutil.copytree(source, tree, symlinks=True)
            # /bin/echo is a real native executable on the test platform as well.
            suite.correctness(tree, source, files)
            names = suite.os.listdir(tree)
            with mock.patch.object(suite.os, "listdir", return_value=names + [names[0]]):
                with self.assertRaisesRegex(BenchmarkError, "repeat"):
                    suite.correctness(tree, source, files)
            (tree / "nested/child/payload").write_bytes(b"corrupt")
            with self.assertRaises(BenchmarkError):
                suite.correctness(tree, source, files)
            (tree / "namespace").chmod(0o644)
            (tree / "namespace").unlink()
            with self.assertRaises(BenchmarkError):
                suite.correctness(tree, source, files)

    def test_busy_unmount_cannot_be_accepted_if_detached(self):
        with tempfile.TemporaryDirectory() as directory:
            file = Path(directory) / "file"
            file.write_bytes(b"ok")
            with mock.patch.object(suite.subprocess, "run", return_value=mock.Mock(returncode=0)), \
                 mock.patch.object(suite.os.path, "ismount", return_value=False):
                with self.assertRaises(BenchmarkError):
                    suite.busy_unmount(Path(directory), file)



if __name__ == "__main__":
    unittest.main()
