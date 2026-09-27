import io
import pathlib
import shutil
import tempfile
import unittest
import zlib

from benchmarks.suites import git as benchmark

class GeneratorTests(unittest.TestCase):
    def test_payloads_are_repeatable_and_delta_shape_keeps_stable_body(self):
        first = benchmark.blob_payload(3, 7, 4096, True)
        second = benchmark.blob_payload(3, 7, 4096, True)
        changed = benchmark.blob_payload(3, 8, 4096, True)
        self.assertEqual(first, second)
        self.assertNotEqual(first, changed)
        self.assertEqual(first[128:], changed[128:])

    def test_fast_import_data_is_binary_safe(self):
        stream = io.BytesIO()
        benchmark.write_fast_import_data(stream, b"a\x00b\n")
        self.assertEqual(stream.getvalue(), b"data 4\na\x00b\n\n")

    def test_pack_heavy_payload_does_not_compress_away(self):
        payload = benchmark.blob_payload(3, 7, 256 * 1024, False, True)
        self.assertGreater(len(zlib.compress(payload)), len(payload) * 0.99)

    def test_smoke_repository_is_valid_and_incremental(self):
        git = shutil.which("git")
        if git is None:
            self.skipTest("git is required")
        scale = benchmark.GitScale(3, 4, 128, 2, 1, True)
        with tempfile.TemporaryDirectory() as directory:
            repository = pathlib.Path(directory) / "source.git"
            benchmark.create_source(git, repository, scale)
            before = benchmark.source_metrics(git, repository)["reachable_objects"]
            benchmark.append_history(git, repository, scale, first_commit=3, commit_count=1)
            after = benchmark.source_metrics(git, repository)["reachable_objects"]
            self.assertGreater(after, before)


class MetricsTests(unittest.TestCase):
    def test_import_command_can_omit_newer_cache_limit_flag(self):
        command = benchmark.casita_import_command(
            pathlib.Path("casita"),
            pathlib.Path("repository"),
            pathlib.Path("source.git"),
            None,
        )

        self.assertNotIn("--git-max-cached-pack-bytes", command.steps[0])

    def test_import_command_records_explicit_cache_limit(self):
        command = benchmark.casita_import_command(
            pathlib.Path("casita"),
            pathlib.Path("repository"),
            pathlib.Path("source.git"),
            123,
        )

        self.assertEqual(command.steps[0][-2:], ["--git-max-cached-pack-bytes", "123"])

    def test_import_metrics(self):
        self.assertEqual(
            benchmark.parse_import_metrics("view git.view.v1:abc\nobjects 42\nrevision 7\n"),
            {"view": "git.view.v1:abc", "objects": 42, "revision": "7"},
        )

    def test_huge_delta_profile_is_about_thirty_gib(self):
        scale = benchmark.SCALES["huge"]["delta-heavy"]
        self.assertGreater(scale.logical_blob_bytes, 30 * 1024**3)
        self.assertLess(scale.logical_blob_bytes, 31 * 1024**3)

    def test_huge_pack_profile_exceeds_thirty_gib(self):
        scale = benchmark.SCALES["huge"]["pack-heavy"]
        self.assertTrue(scale.incompressible)
        self.assertGreater(scale.base_logical_blob_bytes, 32 * 1024**3)
        self.assertGreater(scale.logical_blob_bytes, 30 * 1024**3)

    def test_huge_many_object_profile_generates_over_ten_million_objects(self):
        scale = benchmark.SCALES["huge"]["many-objects"]
        blob_versions = scale.files + (scale.commits - 1) * scale.changes_per_commit
        lower_bound = blob_versions + scale.commits
        self.assertGreater(lower_bound, 10_000_000)

    def test_huge_wide_tree_profile_has_five_million_paths(self):
        self.assertGreaterEqual(benchmark.SCALES["huge"]["wide-tree"].files, 5_000_000)


if __name__ == "__main__":
    unittest.main()
