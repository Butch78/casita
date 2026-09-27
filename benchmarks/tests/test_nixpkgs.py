import pathlib
import subprocess
import tempfile
import unittest
from unittest import mock

from benchmarks.suites import nixpkgs
from benchmarks.suites import repository


class NixpkgsCorpusTests(unittest.TestCase):
    def test_materialization_is_pinned_to_the_commit_and_excludes_worktree_residue(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "source"
            source.mkdir()
            subprocess.run(
                ["git", "init", "--quiet", "--initial-branch", "main"],
                cwd=source,
                check=True,
            )
            subprocess.run(
                ["git", "config", "user.name", "Benchmark Test"], cwd=source, check=True
            )
            subprocess.run(
                ["git", "config", "user.email", "benchmark@invalid"],
                cwd=source,
                check=True,
            )
            (source / "pkgs").mkdir()
            (source / "pkgs" / "hello.nix").write_text("committed\n")
            (source / "default.nix").write_text("{}\n")
            (source / "current").symlink_to("default.nix")
            subprocess.run(["git", "add", "."], cwd=source, check=True)
            subprocess.run(
                ["git", "commit", "--quiet", "-m", "fixture"], cwd=source, check=True
            )
            expected_commit = subprocess.run(
                ["git", "rev-parse", "HEAD"],
                cwd=source,
                check=True,
                capture_output=True,
                text=True,
            ).stdout.strip()

            (source / "default.nix").write_text("dirty\n")
            (source / "untracked").write_text("ignored\n")
            corpus, metadata = repository.materialize_nixpkgs_corpus(
                source, "HEAD", root / "export"
            )

            self.assertEqual((corpus.base / "default.nix").read_text(), "{}\n")
            self.assertFalse((corpus.base / "untracked").exists())
            self.assertEqual(metadata["revision"], expected_commit)
            self.assertEqual(metadata["paths"], len(corpus.base_manifest))
            self.assertEqual(metadata["manifest_sha256"], repository.manifest_identity(corpus.base_manifest))
            repository.assert_manifest(corpus.base, corpus.base_manifest)

    def test_nixpkgs_rejects_operations_without_a_committed_edited_tree(self):
        parser = repository.build_parser()
        args = parser.parse_args(
            ["--source", ".", "--operations", "cold-import,edited-import"]
        )
        with self.assertRaisesRegex(repository.BenchmarkError, "edited-state operations"):
            repository.normalize_args(args)

    def test_source_path_is_redacted_from_recorded_argv(self):
        self.assertEqual(
            repository.redacted_argv(
                ["benchmark", "run", "nixpkgs", "--source", "/private/nixpkgs"]
            ),
            [
                "benchmark",
                "run",
                "nixpkgs",
                "--source",
                "<local-nixpkgs-checkout>",
            ],
        )

    def test_entrypoint_supplies_full_acceptance_defaults_and_allows_overrides(self):
        with mock.patch.object(repository, "main", return_value=0) as run:
            self.assertEqual(
                nixpkgs.main(
                    [
                        "--source",
                        "/tmp/nixpkgs",
                        "--repetitions",
                        "1",
                        "--output",
                        "/tmp/result.json",
                    ]
                ),
                0,
            )
        arguments = run.call_args.args[0]
        self.assertIn(nixpkgs.DEFAULT_OPERATIONS, arguments)
        self.assertIn(nixpkgs.DEFAULT_IMPLEMENTATIONS, arguments)
        self.assertEqual(arguments[-4:], ["--repetitions", "1", "--output", "/tmp/result.json"])

    def test_report_keeps_throughput_and_storage_breakdown_together(self):
        result = {
            "schema_version": 1,
            "environment": {"casita_revision": "abc"},
            "tools": {},
            "samples": [],
            "corpora": {
                "nixpkgs": {
                    "revision": "1" * 40,
                    "tree": "2" * 40,
                    "paths": 10,
                    "logical_file_bytes": 4096,
                }
            },
            "aggregates": [
                {
                    "corpus": "nixpkgs",
                    "cache_policy": "warm",
                    "operation": "checkout",
                    "implementation": "casita",
                    "samples": 1,
                    "median_wall_seconds": 1.0,
                    "p95_wall_seconds": 1.0,
                    "median_throughput_bytes_per_second": 4096.0,
                    "median_max_rss_bytes": 1024.0,
                    "median_repository_allocated_bytes": 3072.0,
                    "median_storage_metrics": {
                        "pack_count": 1.0,
                        "pack_bytes": 2048.0,
                        "blob_allocated_bytes": 2560.0,
                        "metadata_allocated_bytes": 512.0,
                        "loose_chunk_count": 0.0,
                    },
                }
            ],
        }

        report = repository.render_report(result)
        self.assertIn("Median throughput", report)
        self.assertIn("## Storage shape", report)
        self.assertIn("2.0 KiB", report)


if __name__ == "__main__":
    unittest.main()
