import pathlib
import tempfile
import unittest

from benchmarks.suites.git_fetch_s3 import fixture, git, phase_stats, checked_fixture


class GitFetchS3Tests(unittest.TestCase):
    def test_retained_fixture_rejects_a_different_tree(self):
        cached = {"identity": {"tree": "one"}, "setup": {"mode": "import", "reused": False}}
        self.assertTrue(checked_fixture(cached, {"tree": "one"})["reused"])
        with self.assertRaisesRegex(ValueError, "identity differs"):
            checked_fixture(cached, {"tree": "two"})
        self.assertFalse(cached["setup"]["reused"])

    def test_diagnostics_separate_first_and_repeat_fetch(self):
        first = dict(chunk_range_requests=10, chunk_range_bytes=100,
                     whole_pack_requests=0, whole_pack_bytes=0, cache_hits=0,
                     cache_evictions=3, span_totals={"git.fetch.streaming_entry": [6, 0.5]})
        second = {**first, "cache_hits": 10,
                  "span_totals": {"git.fetch.streaming_entry": [12, 0.75]}}
        self.assertEqual(phase_stats({}, first), first)
        delta = phase_stats(first, second)
        self.assertEqual(delta["chunk_range_requests"], 0)
        self.assertEqual(delta["cache_hits"], 10)
        self.assertEqual(delta["span_totals"]["git.fetch.streaming_entry"], [6, 0.25])

    def test_boundary_fixture_is_deterministic_and_spans_threshold(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            identities = []
            for name in ("one", "two"):
                work = root / name
                work.mkdir()
                source, identity = fixture(work)
                identities.append(identity)
                git(source, "fsck", "--full", "--strict")
            self.assertEqual(identities[0], identities[1])
            self.assertEqual(identities[0]["objects"], 12)
            self.assertEqual(identities[0]["blobs_over_1mib"], 6)
            self.assertEqual(identities[0]["largest_blob_bytes"], 16777216)

    def test_snapshot_preserves_tree_without_source_history_or_ref_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            original_work = root / "original"
            original_work.mkdir()
            original, identity = fixture(original_work)
            tip = git(original, "-c", "user.name=Benchmark", "-c", "user.email=bench@example.invalid",
                      "commit-tree", identity["tree"], "-p", identity["commit"], input=b"second\n")
            git(original, "update-ref", "refs/heads/main", tip)
            snapshot_work = root / "snapshot"
            snapshot_work.mkdir()
            snapshot, selected = fixture(snapshot_work, original)
            self.assertEqual(selected["revision"], tip)
            self.assertEqual(selected["tree"], identity["tree"])
            self.assertEqual(git(snapshot, "rev-list", "--count", "HEAD"), "1")
            self.assertEqual(git(original, "rev-parse", "HEAD"), tip)
            git(snapshot, "fsck", "--full", "--strict")
