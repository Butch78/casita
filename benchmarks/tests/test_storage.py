import json
import os
import pathlib
import tarfile
import tempfile
import unittest
from unittest import mock

from benchmarks import cli, storage
from benchmarks import all as runner


class StorageTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = pathlib.Path(self.temporary.name)
        patch = mock.patch.object(cli, "ROOT", self.root)
        patch.start()
        self.addCleanup(patch.stop)

    def test_shared_artifacts_survive_source_rebuild_and_detect_corruption(self):
        source = self.root / "source"
        source.write_bytes(b"original executable")
        first = storage.retain_binary(source, self.root / "first")
        second = storage.retain_binary(source, self.root / "second")
        self.assertEqual(first["shared_path"], second["shared_path"])
        self.assertTrue((self.root / "first").samefile(self.root / "second"))
        source.write_bytes(b"rebuilt executable")
        self.assertEqual((self.root / "first").read_bytes(), b"original executable")
        cached = pathlib.Path(first["shared_path"])
        cached.chmod(0o755)
        cached.write_bytes(b"corrupted")
        source.write_bytes(b"original executable")
        with self.assertRaisesRegex(RuntimeError, "corrupt"):
            storage.retain_binary(source, self.root / "third")

    def test_archive_round_trip_preserves_results_links_and_empty_directories(self):
        source = self.root / "run"
        source.mkdir()
        (source / "empty").mkdir()
        (source / "result.json").write_text('{"samples": [1, 2, 3]}')
        (source / "link").symlink_to("result.json")
        os.link(source / "result.json", source / "hardlink")
        destination = self.root / "run.tar.gz"
        extract = tarfile.TarFile.extractfile

        def extract_regular(bundle, member):
            self.assertFalse(member.islnk(), "hard links must reuse verified target hashes")
            return extract(bundle, member)

        with mock.patch.object(tarfile.TarFile, "extractfile", extract_regular):
            result = storage.archive(source, destination)
        self.assertTrue(result["verified"])
        self.assertTrue((source / "result.json").exists())
        with tarfile.open(destination) as bundle:
            self.assertEqual(bundle.extractfile("./result.json").read(), (source / "result.json").read_bytes())
            self.assertTrue(bundle.getmember("./empty").isdir())
            self.assertEqual(bundle.getmember("./link").linkname, "result.json")
            self.assertEqual(bundle.extractfile("./hardlink").read(), (source / "result.json").read_bytes())
        with self.assertRaises(ValueError):
            storage.archive(source, destination)
        with self.assertRaises(ValueError):
            storage.archive(source, source / "nested.tar.gz")

    def test_clean_only_finished_marked_work_and_never_follows_links(self):
        output = self.root / "results"
        output.mkdir()
        (output / "measurement.json").write_text("{}")
        work = storage.prepare_work(output)
        (work / "fixture").write_bytes(b"disposable")
        (work / "external").symlink_to(output, target_is_directory=True)
        ledger = {"work_directory": str(work), "finished": False}
        (output / "execution.json").write_text(json.dumps(ledger))
        self.assertEqual(storage.clean(True)["directories"], [])
        ledger["finished"] = True
        (output / "execution.json").write_text(json.dumps(ledger))
        self.assertEqual(len(storage.clean()["directories"]), 1)
        self.assertTrue(work.exists())
        removed = storage.clean(True)
        self.assertTrue(pathlib.Path(removed["directories"][0]["archive"]).exists())
        self.assertFalse(work.exists())
        self.assertTrue((output / "measurement.json").exists())

    def test_failed_archive_prevents_cleanup(self):
        output = self.root / "results"
        output.mkdir()
        work = storage.prepare_work(output)
        (output / "execution.json").write_text(json.dumps({"finished": True, "work_directory": str(work)}))
        with mock.patch.object(storage, "archive", side_effect=ValueError("verification failed")):
            with self.assertRaises(ValueError):
                storage.clean(True)
        self.assertTrue(work.exists())

    def test_inventory_does_not_follow_external_links(self):
        results = self.root / "benchmarks/results/run"
        results.mkdir(parents=True)
        (results / "measurement.json").write_bytes(b"{}")
        (results / "outside").symlink_to(self.root, target_is_directory=True)
        reports = self.root / "benchmarks/reports"
        reports.mkdir()
        (reports / "report.md").write_text("See benchmarks/results/run/measurement.json")
        result = storage.inventory(results.parent)
        self.assertEqual(result["bytes"], 2)
        self.assertEqual(result["runs"]["run"]["symlinks"], 1)
        self.assertEqual(result["runs"]["run"]["references"], ["benchmarks/reports/report.md"])

    def test_build_selection_limits_cargo_targets(self):
        commands = runner.build_commands(["transfer-holds"], self.root)
        self.assertEqual(len(commands), 1)
        self.assertIn("--bench", commands[0])
        self.assertIn("transfer_holds", commands[0])
        self.assertNotIn("--examples", commands[0])
        commands = runner.build_commands(["git-fetch-s3", "git-pack-cached"], self.root)
        self.assertEqual(len(commands), 1)
        self.assertEqual(commands[0].count("--example"), 2)
        self.assertNotIn("--lib", commands[0])

    def test_automatic_directories_are_unique(self):
        first, second = storage.new_output(), storage.new_output()
        self.assertNotEqual(first, second)
        self.assertEqual(first.parent, self.root / "benchmarks/results")

    def test_default_output_and_group_selection_reach_runner(self):
        output = self.root / "auto-output"
        with mock.patch.object(storage, "new_output", return_value=output), \
             mock.patch.object(runner.common, "environment_metadata", return_value={}), \
             mock.patch.object(runner.common, "run_checked", return_value="revision"), \
             mock.patch.object(runner, "build_binaries", side_effect=RuntimeError("stop before running")) as build:
            with self.assertRaisesRegex(RuntimeError, "stop before running"):
                runner.main(["--groups", "native-git"])
        expected = [entry["id"] for entry in cli.entrypoints() if entry["suite_id"] == "native-git"]
        self.assertEqual(build.call_args.args[2], expected)
        ledger = json.loads((output / "execution.json").read_text())
        self.assertFalse(ledger["finished"])
        self.assertEqual(storage.clean(True)["directories"], [])


if __name__ == "__main__":
    unittest.main()
