import json
import pathlib
import shutil
import subprocess
import tempfile
import unittest

from benchmarks import dashboard, metrics

ROOT = pathlib.Path(__file__).parents[2]
MANIFEST = ROOT / "benchmarks" / "manifest.json"
BASELINE = ROOT / "benchmarks" / "baselines" / "2026-08-17-3b38afc-linux-x86_64.json"


class ManifestTests(unittest.TestCase):
    def test_manifest_covers_every_north_star_dimension(self):
        manifest = dashboard.load_manifest(MANIFEST)
        covered = {dimension for suite in manifest["suites"] for dimension in suite["dimensions"]}
        self.assertEqual(covered, set(manifest["dimensions"]))
        self.assertIn("native-git", {suite["id"] for suite in manifest["suites"]})
        self.assertIn("huge-repositories", {suite["id"] for suite in manifest["suites"]})
        self.assertEqual(len(manifest["frontiers"]), 9)
        self.assertEqual(
            {frontier["axis"] for frontier in manifest["frontiers"]},
            {
                "physical_bytes",
                "objects",
                "paths",
                "revisions",
                "generations",
                "change_ratio_ppm",
                "memory_budget_bytes",
                "restoration",
            },
        )

    def test_manifest_rejects_invalid_acceptance_budgets(self):
        manifest = json.loads(MANIFEST.read_text())
        manifest["entrypoints"][0]["budgets"] = {"requests": True}
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "manifest.json"
            path.write_text(json.dumps(manifest))
            with self.assertRaises(dashboard.DashboardError):
                dashboard.load_manifest(path)


class CatalogTests(unittest.TestCase):
    def test_legacy_repository_baseline_normalizes(self):
        manifest = dashboard.load_manifest(MANIFEST)
        catalog = dashboard.build_catalog(manifest, [BASELINE])
        dashboard.validate_catalog(catalog)
        self.assertEqual(catalog["runs"][0]["suite_id"], "repository-e2e")
        self.assertEqual(len(catalog["runs"][0]["observations"]), 210)
        metrics = catalog["runs"][0]["observations"][0]["metrics"]
        self.assertIn("wall_seconds", metrics)
        self.assertIn("max_rss_bytes", metrics)

    def test_nixpkgs_storage_breakdown_reaches_dashboard_metrics(self):
        raw = {
            "suite_id": "repository-e2e",
            "result_schema": "casita.repository-e2e.v1",
            "schema_version": 1,
            "environment": {"casita_revision": "abc", "casita_worktree_dirty": True},
            "configuration": {"profile": "standard"},
            "tools": {},
            "corpora": {"nixpkgs": {"base_bytes": 4096}},
            "samples": [],
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
                    "median_operation_metrics": {"pack_chunk_range_requests": 17.0},
                    "median_storage_metrics": {
                        "pack_bytes": 2048.0,
                        "metadata_allocated_bytes": 512.0,
                        "loose_chunk_count": 0.0,
                    },
                }
            ],
        }
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "nixpkgs.json"
            path.write_text(json.dumps(raw))
            run = dashboard.normalize_result(path)

        metrics = run["observations"][0]["metrics"]
        self.assertEqual(metrics["throughput_bytes_per_second"], 4096.0)
        self.assertEqual(metrics["pack_bytes"], 2048.0)
        self.assertEqual(metrics["metadata_allocated_bytes"], 512.0)
        self.assertEqual(metrics["loose_chunk_count"], 0.0)
        self.assertEqual(metrics["pack_chunk_range_requests"], 17.0)

    def test_native_git_failures_remain_visible(self):
        raw = {
            "suite_id": "native-git",
            "result_schema": "casita.native-git.v1",
            "schema_version": 1,
            "environment": {"casita_revision": "abc", "casita_worktree_dirty": True},
            "configuration": {"profile": "smoke", "shape": "many-objects", "cache_policy": "warm"},
            "tools": {},
            "corpora": [{"logical_blob_bytes": 10, "base": {"reachable_objects": 4, "pack_bytes": 3}}],
            "samples": [
                {"operation": "full-clone", "status": "ok", "wall_seconds": 1.0, "max_rss_bytes": 20},
                {"operation": "git-full-clone", "status": "ok", "wall_seconds": 0.5, "max_rss_bytes": 10},
                {"operation": "incremental-fetch", "status": "failed", "error": "expected ACK"},
            ],
        }
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "git.json"
            path.write_text(json.dumps(raw))
            run = dashboard.normalize_result(path)
        failure = next(row for row in run["observations"] if row["operation"] == "incremental-fetch")
        self.assertEqual(failure["status"], "failed")
        self.assertIn("expected ACK", failure["failures"][0])

    def test_gix_odb_result_normalizes_latency_and_pack_shape(self):
        raw = {
            "suite_id": "native-git",
            "result_schema": "casita.gix-odb.v1",
            "schema_version": 1,
            "environment": {"casita_revision": "abc", "casita_worktree_dirty": True},
            "configuration": {
                "profile": "smoke",
                "scale": {"objects": 512, "body_bytes": 4096},
            },
            "tools": {},
            "samples": [
                {
                    "implementation": "casita",
                    "backend": "local",
                    "operation": "warm-find",
                    "status": "ok",
                    "wall_seconds": 0.1,
                    "nanos_per_op": 200,
                    "throughput_bytes_per_second": 10_000,
                    "process_max_rss_bytes": 1024,
                    "pack": {"cache_hits": 512, "chunk_range_requests": 0},
                }
            ],
        }
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "gix-odb.json"
            path.write_text(json.dumps(raw))
            run = dashboard.normalize_result(path)
        observation = run["observations"][0]
        self.assertEqual(run["suite_id"], "native-git")
        self.assertEqual(observation["metrics"]["nanos_per_op"], 200)
        self.assertEqual(observation["metrics"]["pack_cache_hits"], 512)
        self.assertEqual(observation["scale"]["logical_bytes"], 512 * 4096)

    def test_graph_traversal_result_normalizes_spill_budgets(self):
        raw = {
            "suite_id": "graph-traversal",
            "schema_version": 1,
            "environment": {"casita_revision": "abc", "casita_worktree_dirty": True},
            "profile": "standard",
            "samples": [
                {
                    "status": "ok",
                    "operation": "verify-closure",
                    "objects": 2048,
                    "spill_bytes_budget": 1024,
                    "spill_memory_objects": 4,
                    "spill_files_opened": 2,
                    "spill_peak_bytes": 512,
                    "wall_seconds": 1.0,
                    "max_rss_bytes": 20,
                }
            ],
        }
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "graph.json"
            path.write_text(json.dumps(raw))
            run = dashboard.normalize_result(path)
        observation = run["observations"][0]
        self.assertEqual(run["suite_id"], "graph-traversal")
        self.assertEqual(observation["metrics"]["spill_bytes_budget"], 1024)
        self.assertEqual(observation["metrics"]["spill_files_opened"], 2.0)
        self.assertEqual(observation["scale"]["objects"], 2048)

    def test_s3_path_transfer_normalizes_latency_and_request_shape(self):
        phase = {
            "wall_nanos": 400_000_000,
            "published_objects": 17,
            "payloads_sent": 17,
            "chunks_sent": 0,
            "pack_chunk_range_requests": 18,
            "pack_chunk_range_bytes": 66_380,
            "pack_whole_requests": 0,
            "pack_whole_bytes": 0,
            "pack_cache_hits": 0,
            "pack_cache_promotions": 0,
            "wal_manifest_refresh_requests": 1,
            "wal_fragment_get_requests": 0,
        }
        raw = {
            "suite_id": "transfer",
            "result_schema": "casita.s3-path-transfer.v4",
            "generated_at": "2026-08-29T00:00:00+00:00",
            "environment": {"casita_revision": "abc", "casita_worktree_dirty": False},
            "configuration": {"repetitions": 1},
            "samples": [
                {
                    "transport": "direct-s3",
                    "rtt_ms": 80,
                    "depth": 4,
                    "subtree_files": 16,
                    "cache_mib": 0,
                    "cold": phase,
                    "warm": {**phase, "wall_nanos": 300_000_000},
                    "process_resources": {"max_rss_bytes": 123_456_789},
                }
            ],
        }
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "s3-path.json"
            path.write_text(json.dumps(raw))
            run = dashboard.normalize_result(path)
        self.assertEqual(run["suite_id"], "transfer")
        self.assertEqual(run["release_status"], "release")
        self.assertEqual(run["captured_at_utc"], raw["generated_at"])
        self.assertEqual(len(run["observations"]), 2)
        cold = next(row for row in run["observations"] if row["cache_policy"] == "cold")
        self.assertEqual(cold["workload"], "depth-4-files-16-rtt-80ms")
        self.assertEqual(cold["metrics"]["wall_seconds"], 0.4)
        self.assertEqual(cold["metrics"]["pack_chunk_range_requests"], 18.0)
        self.assertEqual(cold["metrics"]["max_rss_bytes"], 123_456_789)
        self.assertEqual(cold["scale"]["path_depth"], 4)

    def test_pack_result_schemas_route_to_their_registered_suites(self):
        cases = [
            (
                "casita.pack-limits.v1",
                {
                    "observations": [
                        {
                            "corpus": "small-files",
                            "target_mib": 4,
                            "operation": "checkout",
                            "median_wall_seconds": 0.1,
                        }
                    ]
                },
                "blob-backends",
            ),
            (
                "casita.pack-index.v1",
                {
                    "configuration": {"files": 8, "pack_target_mib": 4},
                    "cold": {"exit_code": 0, "wall_seconds": 0.1, "metrics": {}},
                },
                "blob-backends",
            ),
            (
                "casita.catalog-index.v1",
                {"samples": [{"entries": 1024, "budget_checks": []}]},
                "blob-backends",
            ),
            (
                "casita.pack-gc.v1",
                {
                    "samples": [
                        {
                            "target_mib": 4,
                            "requested_dead_percent": 10,
                            "exit_code": 0,
                            "gc_metrics": {},
                            "operation_metrics": {},
                        }
                    ]
                },
                "collection-and-fsck",
            ),
            (
                "casita.s3-pack.v1",
                {
                    "samples": [
                        {
                            "target_mib": 4,
                            "cache_mib": 0,
                            "promotion_reads": 2,
                            "exit_code": 0,
                            "operation_metrics": {},
                        }
                    ]
                },
                "blob-backends",
            ),
            (
                "casita.s3-pack-gc.v2",
                {
                    "samples": [
                        {
                            "target_mib": 4,
                            "requested_dead_percent": 10,
                            "metrics": {},
                            "request_ledger": {},
                            "budget_checks": [],
                        }
                    ]
                },
                "collection-and-fsck",
            ),
            (
                "casita.s3-pack-index.v5",
                {
                    "samples": [
                        {"files": 8192, "target_mib": 16, "metrics": {}, "budget_checks": []}
                    ]
                },
                "blob-backends",
            ),
        ]
        with tempfile.TemporaryDirectory() as directory:
            for index, (schema, body, suite_id) in enumerate(cases):
                with self.subTest(schema=schema):
                    path = pathlib.Path(directory) / f"result-{index}.json"
                    path.write_text(json.dumps({"result_schema": schema, **body}))
                    run = dashboard.normalize_result(path)
                    self.assertEqual(run["suite_id"], suite_id)
                    self.assertEqual(len(run["observations"]), 1)
                    self.assertEqual(run["release_status"], "development")

    def test_s3_pack_index_exposes_cost_and_latency_metrics_for_comparison(self):
        raw = {
            "result_schema": "casita.s3-pack-index.v5",
            "environment": {"casita_revision": "abc", "casita_worktree_dirty": False},
            "samples": [
                {
                    "files": 8192,
                    "target_mib": 16,
                    "max_rss_bytes": 234_567_890,
                    "metrics": {
                        "cold_wall_nanos": 20_000_000,
                        "warm_wall_nanos": 10_000_000,
                        "warm_index_pointer_requests": 0,
                        "warm_index_requests": 0,
                        "warm_list_requests": 0,
                        "warm_footer_range_requests": 0,
                    },
                    "request_ledger": {
                        "warm_open": {"total": 3},
                        "warm_first_snapshot": {"total": 1},
                        "warm_repeat_snapshot": {"total": 1},
                    },
                    "budget_checks": [],
                }
            ],
        }
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "s3-pack-index.json"
            path.write_text(json.dumps(raw))
            run = dashboard.normalize_result(path)

        observed = run["observations"][0]["metrics"]
        self.assertEqual(observed["cold_wall_seconds"], 0.02)
        self.assertEqual(observed["warm_wall_seconds"], 0.01)
        self.assertEqual(observed["warm_open_total_requests"], 3)
        self.assertEqual(observed["warm_first_snapshot_total_requests"], 1)
        self.assertEqual(observed["warm_repeat_snapshot_total_requests"], 1)
        self.assertEqual(observed["max_rss_bytes"], 234_567_890)
        registry = metrics.load_metric_registry(dashboard.load_manifest(MANIFEST))
        registered = metrics.registered_metrics(run["observations"][0], registry)
        self.assertEqual(
            set(registered),
            {
                "cold_wall_seconds",
                "warm_wall_seconds",
                "warm_open_total_requests",
                "warm_first_snapshot_total_requests",
                "warm_repeat_snapshot_total_requests",
                "warm_index_pointer_requests",
                "warm_index_requests",
                "warm_list_requests",
                "warm_footer_range_requests",
                "max_rss_bytes",
            },
        )

    def test_dashboard_script_parses(self):
        node = shutil.which("node")
        if node is None:
            self.skipTest("node is required")
        catalog = dashboard.build_catalog(dashboard.load_manifest(MANIFEST), [BASELINE])
        page = dashboard.render_html(catalog)
        opening = page.index("<script>", page.index('<script id="catalog"'))
        script = page[opening + len("<script>") : page.index("</script>", opening)]
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "dashboard.mjs"
            path.write_text(script)
            result = subprocess.run([node, "--check", str(path)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Whole-system coverage", page)
        self.assertIn("Huge means multiple cliffs", page)
        self.assertIn("500 TB physical repository", page)
        self.assertIn("30 GiB physical repository", page)
        self.assertIn("All suites", page)
        self.assertIn("All metrics", page)
        self.assertIn("benchmark results", page)
        self.assertIn("operationRows.flatMap(row=>Object.keys(row.metrics))", page)
        self.assertIn("comparisonGroups(selected)", page)
        self.assertIn("comparable cohorts", page)
        self.assertIn("comparableMetrics(group)", page)
        self.assertIn("grouped by metric", page)
        self.assertIn("lower is better", page)
        self.assertIn("warm OS cache", page)
        self.assertIn("cold OS cache", page)
        self.assertIn("entrypoints[id]?.title", page)
        self.assertIn("quick validation run", page)
        self.assertIn("operation.addEventListener('change',cascade)", page)


if __name__ == "__main__":
    unittest.main()
