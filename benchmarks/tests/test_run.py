import pathlib
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock

from benchmarks.suites import repository as benchmark

def rendered_result() -> dict:
    samples = []
    for repetition, wall in enumerate((1.0, 2.0, 3.0), start=1):
        samples.append(
            {
                "status": "ok",
                "corpus": "small-files",
                "cache_policy": "warm",
                "operation": "cold-import",
                "implementation": "casita",
                "repetition": repetition,
                "wall_seconds": wall,
                "throughput_bytes_per_second": 100 / wall,
                "max_rss_bytes": 1024,
                "stdout": {"text": "", "truncated": False},
                "stderr": {"text": "", "truncated": False},
                "operation_metrics": {},
                "storage_metrics": {
                    "pack_count": 2,
                    "pack_bytes": 1024,
                    "blob_allocated_bytes": 1536,
                    "metadata_allocated_bytes": 512,
                    "loose_chunk_count": 0,
                },
                "repository_usage": {"allocated_bytes": 2048},
            }
        )
    return {
        "schema_version": 1,
        "environment": {"casita_revision": "abc"},
        "tools": {"casita": {"status": "available", "version": "casita 0.1"}},
        "samples": samples,
        "aggregates": benchmark.aggregates(samples),
    }


class CorpusTests(unittest.TestCase):
    def test_generation_is_repeatable_and_edit_is_visible(self):
        scale = benchmark.CorpusScale(8, 256, 2, 1024)
        with tempfile.TemporaryDirectory() as first_dir, tempfile.TemporaryDirectory() as second_dir:
            first = benchmark.generate_corpus(pathlib.Path(first_dir), "mixed", scale)
            second = benchmark.generate_corpus(pathlib.Path(second_dir), "mixed", scale)
            self.assertEqual(first.base_manifest, second.base_manifest)
            self.assertEqual(first.edited_manifest, second.edited_manifest)
            self.assertNotEqual(first.base_manifest, first.edited_manifest)
            benchmark.assert_manifest(first.base, first.base_manifest)
            benchmark.assert_manifest(first.edited, first.edited_manifest)

    def test_manifest_rejects_changed_bytes(self):
        scale = benchmark.CorpusScale(1, 128, 0, 0)
        with tempfile.TemporaryDirectory() as directory:
            corpus = benchmark.generate_corpus(pathlib.Path(directory), "small-files", scale)
            target = next(path for path in corpus.base.rglob("*") if path.is_file())
            target.write_bytes(b"different")
            with self.assertRaises(benchmark.BenchmarkError):
                benchmark.assert_manifest(corpus.base, corpus.base_manifest)

    def test_git_snapshot_adapter_includes_files_matched_by_source_gitignore(self):
        git = shutil.which("git")
        if git is None:
            self.skipTest("git is required for the comparator adapter")
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source = root / "source"
            source.mkdir()
            (source / ".gitignore").write_text("ignored\n")
            (source / "ignored").write_text("committed in the source snapshot\n")
            repository = root / "repository"
            adapter = benchmark.GitAdapter(git, pack_after_ingest=True)
            adapter.init(repository)
            adapter.ingest(repository, source, "snapshot")
            tracked = benchmark.run_checked(
                [git, f"--git-dir={repository / '.git'}", "ls-files"]
            ).splitlines()
            self.assertIn("ignored", tracked)
            metrics = adapter.storage_metrics([repository / ".git"])
            self.assertEqual(metrics["pack_count"], 1)
            self.assertGreater(metrics["pack_bytes"], 0)
            self.assertEqual(metrics["loose_object_count"], 0)


class ResultTests(unittest.TestCase):
    def test_nearest_rank_percentile(self):
        self.assertEqual(benchmark.percentile([4.0, 1.0, 3.0, 2.0], 0.5), 2.0)
        self.assertEqual(benchmark.percentile(list(range(1, 21)), 0.95), 19)

    def test_balanced_order_rotates_implementations(self):
        class Fake:
            supported_operations = ("cold-import",)

        jobs = benchmark.balanced_jobs(
            ["small-files"], ["cold-import"], ["warm"], {"a": Fake(), "b": Fake(), "c": Fake()}, 3, 73
        )
        orders = [[job[3] for job in jobs[offset : offset + 3]] for offset in (0, 3, 6)]
        self.assertEqual(sorted(orders[0]), ["a", "b", "c"])
        self.assertEqual(orders[1], orders[0][1:] + orders[0][:1])
        self.assertEqual(orders[2], orders[0][2:] + orders[0][:2])

    def test_report_is_derived_from_raw_samples(self):
        result = rendered_result()
        report = benchmark.render_report(result)
        self.assertIn("| small-files | warm | cold-import | casita | 3 | 2.0000 s | 3.0000 s |", report)
        self.assertEqual(result["aggregates"][0]["median_storage_metrics"]["pack_bytes"], 1024)

    def test_operation_metrics_are_aggregated_for_commit_comparison(self):
        result = rendered_result()
        for sample, requests in zip(result["samples"], (9, 7, 8), strict=True):
            sample["operation_metrics"] = {"pack_chunk_range_requests": requests}
        aggregate = benchmark.aggregates(result["samples"])[0]
        self.assertEqual(
            aggregate["median_operation_metrics"]["pack_chunk_range_requests"], 8
        )

        page = benchmark.render_html(result)
        self.assertIn("Performance,", page)
        self.assertIn('id="benchmark-data"', page)
        self.assertIn('"median_wall_seconds":2.0', page)

    def test_report_page_behaviour_script_parses(self):
        # A syntax error anywhere in the emitted script leaves the page with its
        # static numbers but empty filters and no chart, which reads as a data
        # problem rather than a broken build.
        node = shutil.which("node")
        if node is None:
            self.skipTest("node is required to parse the report page script")

        page = benchmark.render_html(rendered_result())
        opening = page.index("<script>", page.index('<script id="benchmark-data"'))
        script = page[opening + len("<script>") :]
        script = script[: script.index("</script>")]

        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "report.mjs"
            path.write_text(script, encoding="utf-8")
            check = subprocess.run([node, "--check", str(path)], capture_output=True, text=True)

        self.assertEqual(check.returncode, 0, check.stderr)

    def test_result_validation_rejects_incomplete_success(self):
        result = {
            "schema_version": 1,
            "environment": {},
            "configuration": {},
            "tools": {},
            "corpora": {},
            "samples": [
                {
                    "execution_index": 1,
                    "corpus": "small-files",
                    "cache_policy": "warm",
                    "operation": "cold-import",
                    "implementation": "casita",
                    "repetition": 1,
                    "source_bytes": 1,
                    "status": "ok",
                }
            ],
            "aggregates": [],
        }
        with self.assertRaises(benchmark.BenchmarkError):
            benchmark.validate_result(result)

    def test_casita_transfer_counters_are_retained(self):
        adapter = benchmark.CasitaAdapter("casita")
        metrics = adapter.operation_metrics(
            "sync-warm",
            "published-objects 3\npayloads-sent 2\npayloads-reused 5\nchunks-sent 7\nchunks-reused 11\npack-whole-requests 2\npack-cache-hits 19\nsource-pack-whole-requests 3\nsource-pack-cache-hits 23\n",
        )
        self.assertEqual(metrics["payloads_reused"], 5)
        self.assertEqual(metrics["chunks_sent"], 7)
        self.assertEqual(metrics["pack_whole_requests"], 2)
        self.assertEqual(metrics["pack_cache_hits"], 19)
        self.assertEqual(metrics["source_pack_whole_requests"], 3)
        self.assertEqual(metrics["source_pack_cache_hits"], 23)

    def test_casita_pack_target_is_a_global_cli_option(self):
        adapter = benchmark.CasitaAdapter("casita", 4096)
        self.assertEqual(
            adapter.command(pathlib.Path("repository"), "init"),
            [
                "casita",
                "--pack-target-bytes",
                "4096",
                "--repository",
                "repository",
                "init",
            ],
        )

    def test_casita_fsck_mode_supports_revision_compatible_dry_runs(self):
        adapter = benchmark.CasitaAdapter("casita", fsck_mode="dry-run")
        self.assertEqual(
            adapter.fsck_command(pathlib.Path("repository")),
            ["casita", "--repository", "repository", "fsck", "--dry-run"],
        )

    def test_incremental_sync_is_explicit_and_only_changes_sync_commands(self):
        ordinary = benchmark.CasitaAdapter("casita")
        incremental = benchmark.CasitaAdapter("casita", incremental_sync=True)
        self.assertEqual(ordinary.cli_command("sync"), ["casita", "sync"])
        self.assertEqual(incremental.cli_command("sync"), ["casita", "sync", "--incremental"])
        self.assertEqual(incremental.cli_command("init"), ["casita", "init"])

    def test_casita_post_validation_fsck_can_be_disabled(self):
        adapter = benchmark.CasitaAdapter("casita", post_validate_fsck=False)
        corpus = mock.Mock(base_manifest={})
        prepared = benchmark.Prepared(
            mock.Mock(),
            [pathlib.Path("repository")],
            [],
            {},
            None,
        )
        with (
            mock.patch.object(benchmark, "run_checked", return_value="00  bench/current") as run,
            mock.patch.object(benchmark, "assert_manifest"),
        ):
            adapter.validate("verify", corpus, pathlib.Path("workspace"), prepared)
        self.assertFalse(any("fsck" in call.args[0] for call in run.call_args_list))

    def test_casita_storage_metrics_read_pack_trailers(self):
        adapter = benchmark.CasitaAdapter("casita")
        with tempfile.TemporaryDirectory() as directory:
            repository = pathlib.Path(directory)
            pack = repository / "blobs" / "packs" / "b3" / "aa" / "digest"
            pack.parent.mkdir(parents=True)
            footer = (2).to_bytes(8, "little")
            pack.write_bytes(b"body" + footer + len(footer).to_bytes(8, "little") + b"casitac1")
            metrics = adapter.storage_metrics([repository])
        self.assertEqual(metrics["pack_count"], 1)
        self.assertEqual(metrics["pack_entries"], 2)
        self.assertEqual(metrics["pack_footer_bytes"], 8)
        self.assertEqual(metrics["rebuild_gets_exact"], 2)
        self.assertEqual(metrics["rebuild_bytes_exact"], 24)
        self.assertEqual(metrics["loose_chunk_count"], 0)
        self.assertIn("blob_allocated_bytes", metrics)
        self.assertIn("metadata_allocated_bytes", metrics)


if __name__ == "__main__":
    unittest.main()


class TimerAndEditRegressionTests(unittest.TestCase):
    def test_native_rss_does_not_inherit_python_heap(self):
        import os
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            spec = benchmark.CommandSpec([["/bin/sh", "-c", "printf '%s' \"$CHECK\"; pwd; exit 7"]], root, {**os.environ, "CHECK": "value"})
            first = benchmark.measured_command(spec, root / "out", root / "err", check=False)
            retained = bytearray(128 * 1024 * 1024)
            second = benchmark.measured_command(spec, root / "out", root / "err", check=False)
            self.assertEqual(len(retained), 128 * 1024 * 1024)
            self.assertEqual(second["exit_code"], 7)
            self.assertIn("value" + str(root.resolve()), (root / "out").read_text())
            self.assertLess(second["max_rss_bytes"], first["max_rss_bytes"] + 32 * 1024 * 1024)

    def test_in_place_delta_preserves_unchanged_file_identities(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            corpus = benchmark.generate_corpus(root, "small-files", benchmark.SCALES["smoke"]["small-files"])
            source = root / "copy"
            shutil.copytree(corpus.base, source, symlinks=True)
            def identities():
                return {name: (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns)
                    for name, entry in corpus.base_manifest.items()
                    if entry["type"] == "file" and corpus.edited_manifest.get(name) == entry
                    for info in [(source / name).stat()]}
            before = identities()
            benchmark.apply_corpus_delta(corpus, source)
            self.assertTrue(before)
            self.assertEqual(identities(), before)
            benchmark.assert_manifest(corpus.base, corpus.base_manifest)
            benchmark.assert_manifest(source, corpus.edited_manifest)
