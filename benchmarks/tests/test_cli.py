import argparse
import contextlib
import io
import json
import pathlib
import unittest

from benchmarks import cli
from benchmarks.lib.budgets import entrypoint_budgets
from benchmarks.suites.pack import catalog


class BenchmarkCliTests(unittest.TestCase):
    def test_manifest_entrypoints_are_unique_and_discoverable(self):
        entries = cli.entrypoints()
        identifiers = [entry["id"] for entry in entries]
        self.assertEqual(len(identifiers), len(set(identifiers)))
        self.assertIn("catalog-index", identifiers)
        self.assertIn("nixpkgs", identifiers)
        self.assertIn("s3-pack-index", identifiers)
        self.assertIn("gix-odb", identifiers)
        self.assertIn("benchmark run <suite>", cli.render_list(entries))

    def test_list_json_is_machine_readable(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            self.assertEqual(cli.main(["list"]), 0)
        self.assertIn("catalog-index", output.getvalue())

    def test_unknown_suite_is_an_error(self):
        errors = io.StringIO()
        with contextlib.redirect_stderr(errors):
            self.assertEqual(cli.main(["run", "missing-suite"]), 2)
        self.assertIn("benchmark list", errors.getvalue())

    def test_comparison_commands_are_advertised(self):
        self.assertIn("export-bencher", cli.usage())
        self.assertIn("compare --base-result", cli.usage())
        self.assertIn("revisions REVISION", cli.usage())


class CatalogRunnerTests(unittest.TestCase):
    def test_probe_parser_ignores_test_harness_output(self):
        metrics = catalog.parse_metrics(
            "running 1 test\ntest blob::pack::benchmarks::probe ... "
            "catalog_entries 65536\ncatalog_bytes 100\nindex_lookup_count 5\nok\n"
        )
        self.assertEqual(metrics["catalog_entries"], 65536)
        self.assertEqual(metrics["index_lookup_count"], 5)

    def test_cargo_artifact_parser_finds_the_lib_test_binary(self):
        artifact = {
            "reason": "compiler-artifact",
            "target": {"name": "casita", "kind": ["lib"]},
            "profile": {"test": True},
            "executable": "/tmp/target/release/deps/casita-1234",
        }
        output = "not json\n" + json.dumps(artifact) + "\n"
        self.assertEqual(
            catalog.parse_probe_binary(output),
            pathlib.Path("/tmp/target/release/deps/casita-1234"),
        )

    @staticmethod
    def sample(
        *,
        entries=1_000_000,
        rss_kib=256 * 1024,
        hit_nanos=1_000,
        miss_nanos=1_000,
        gc_nanos=100_000_000,
    ):
        return {
            "entries": entries,
            "manifest_percent": 0,
            "decode": {"catalog_peak_rss_kib": rss_kib},
            "operations": {
                "index_lookup_hit_nanos_per_op": hit_nanos,
                "index_lookup_miss_nanos_per_op": miss_nanos,
                "index_gc_median_nanos": gc_nanos,
            },
        }

    def test_catalog_scale_budgets_accept_the_limits(self):
        budgets = entrypoint_budgets("catalog-index")
        checks = catalog.evaluate_budgets(self.sample(), {"gc_percent": 10}, budgets)
        self.assertEqual(len(checks), 4)
        self.assertEqual({check["status"] for check in checks}, {"passed"})
        summary = catalog.summarize_budgets([{"budget_checks": checks}])
        self.assertEqual(
            summary,
            {
                "status": "passed",
                "passed": 4,
                "failed": 0,
                "not_applicable": 0,
            },
        )

    def test_catalog_scale_budgets_fail_above_the_limits(self):
        budgets = entrypoint_budgets("catalog-index")
        sample = self.sample(
            rss_kib=256 * 1024 + 1,
            hit_nanos=1_001,
            miss_nanos=1_001,
            gc_nanos=100_000_001,
        )
        checks = catalog.evaluate_budgets(sample, {"gc_percent": 10}, budgets)
        self.assertEqual({check["status"] for check in checks}, {"failed"})
        self.assertEqual(
            catalog.summarize_budgets([{"budget_checks": checks}])["status"],
            "failed",
        )

    def test_catalog_budgets_only_gate_the_canonical_scale_and_gc_shape(self):
        budgets = entrypoint_budgets("catalog-index")
        self.assertEqual(
            catalog.evaluate_budgets(
                self.sample(entries=999_999), {"gc_percent": 10}, budgets
            ),
            [],
        )
        checks = catalog.evaluate_budgets(self.sample(), {"gc_percent": 20}, budgets)
        self.assertEqual(checks[-1]["status"], "not-applicable")
        self.assertEqual(
            catalog.summarize_budgets([{"budget_checks": checks}])["status"],
            "partial",
        )
        manifest_sample = self.sample()
        manifest_sample["manifest_percent"] = 10
        checks = catalog.evaluate_budgets(manifest_sample, {"gc_percent": 10}, budgets)
        self.assertEqual(checks[0]["status"], "passed")
        manifest_sample["decode"]["catalog_peak_rss_kib"] = 512 * 1024
        self.assertEqual(catalog.evaluate_budgets(manifest_sample, {"gc_percent": 10}, budgets)[0]["status"], "failed")

    def test_manifest_percentages_accept_zero_and_deduplicate(self):
        self.assertEqual(catalog.unique_percent_csv("0,10,100,10"), [0, 10, 100])
        with self.assertRaises(argparse.ArgumentTypeError):
            catalog.unique_percent_csv("101")

    def test_rebase_rss_repetitions_are_aggregated_from_isolated_processes(self):
        probes = [
            {
                "catalog_entries": 1_000_000,
                "catalog_rebase_get_requests": 8,
                "catalog_rebase_put_requests": 9,
                "catalog_rebase_median_nanos": nanos,
                "catalog_rebase_peak_rss_kib": rss,
            }
            for nanos, rss in ((30, 150), (10, 170), (20, 160))
        ]
        combined = catalog.aggregate_isolated_rebase_probes(probes)
        self.assertEqual(combined["catalog_rebase_median_nanos"], 20)
        self.assertEqual(combined["catalog_rebase_peak_rss_kib"], 170)
        self.assertEqual(
            combined["catalog_rebase_peak_rss_samples_kib"], [150, 170, 160]
        )
        self.assertEqual(combined["catalog_rebase_isolated_processes"], 3)

        probes[1]["catalog_rebase_get_requests"] = 7
        with self.assertRaisesRegex(RuntimeError, "disagreed on exact metrics"):
            catalog.aggregate_isolated_rebase_probes(probes)

    def test_lazy_sharded_request_and_rss_gates_are_exact(self):
        budgets = entrypoint_budgets("catalog-index")
        sample = {
            "entries": 1_000_000,
            "shard_generate": {
                "catalog_lazy_chunk_shards": 4,
                "catalog_lazy_manifest_shards": 3,
                "catalog_lazy_pack_shards": 2,
            },
            "sharded": {
                "catalog_lazy_open_index_requests": 1,
                "catalog_lazy_open_list_requests": 0,
                "catalog_lazy_open_footer_requests": 0,
                "catalog_lazy_first_lookup_requests": 2,
                "catalog_lazy_first_lookup_bytes": 128 * 1024,
                "catalog_lazy_cached_lookup_requests": 0,
                "catalog_lazy_chunk_list_requests": 4,
                "catalog_lazy_manifest_list_requests": 3,
                "catalog_lazy_gc_requests": 2,
                "catalog_lazy_stream_peak_rss_kib": 256 * 1024,
            },
            "rebase": {
                "catalog_rebase_get_requests": 10,
                "catalog_rebase_old_objects": 9,
                "catalog_rebase_put_requests": 11,
                "catalog_rebase_new_objects": 10,
                "catalog_rebase_peak_rss_kib": 256 * 1024,
            },
        }
        checks = catalog.evaluate_lazy_budgets(sample, budgets)
        self.assertEqual(len(checks), 13)
        self.assertEqual({check["status"] for check in checks}, {"passed"})

        sample["sharded"]["catalog_lazy_cached_lookup_requests"] = 1
        checks = catalog.evaluate_lazy_budgets(sample, budgets)
        self.assertIn("failed", {check["status"] for check in checks})

    def test_catalog_report_renders_delta_publication_tradeoff(self):
        result = {
            "configuration": {"threads": 8, "gc_percent": 10},
            "budget_summary": {"status": "passed"},
            "request_projection": catalog.request_amplification_projection(
                storage_tb=500,
                average_chunk_kib=256,
                pack_mib=16,
                run_mib=1,
            ),
            "rebase_projections": [
                catalog.periodic_rebase_projection(
                    storage_tb=500,
                    average_chunk_kib=256,
                    pack_mib=16,
                    run_mib=1,
                    rebase_mib=rebase_mib,
                )
                for rebase_mib in (64, 256, 1024, 4096)
            ],
            "samples": [
                {
                    "entries": 1_000_000,
                    "manifest_percent": 100,
                    "generate": {"catalog_manifests": 1_000_000, "catalog_bytes": 80},
                    "decode": {
                        "catalog_decode_median_nanos": 1_000_000,
                        "catalog_peak_rss_kib": 1024,
                    },
                    "operations": {
                        "index_lookup_hit_nanos_per_op": 1,
                        "index_lookup_miss_nanos_per_op": 2,
                        "index_manifest_lookup_hit_nanos_per_op": 3,
                        "index_manifest_lookup_miss_nanos_per_op": 4,
                        "index_parallel_lookup_nanos_per_op": 5,
                        "index_list_median_nanos": 6,
                        "index_gc_median_nanos": 7,
                    },
                    "publication": {
                        "catalog_full_rewrite_bytes": 80,
                        "catalog_delta_catalog_bytes": 20,
                        "catalog_full_encode_median_nanos": 8,
                        "catalog_delta_encode_median_nanos": 9,
                        "catalog_delta_reopen_median_nanos": 10,
                        "catalog_delta_reopen_bytes": 100,
                        "catalog_delta_reopen_requests": 2,
                        "catalog_run_batch_deltas": 32,
                        "catalog_run_bytes": 30,
                        "catalog_run_catalog_bytes": 40,
                        "catalog_run_seal_median_nanos": 12,
                        "catalog_run_reopen_median_nanos": 13,
                        "catalog_run_reopen_requests": 3,
                        "catalog_shard_bits": 12,
                        "catalog_shard_map_bytes": 100,
                        "catalog_shard_objects": 3,
                        "catalog_shard_chunk_objects": 1,
                        "catalog_shard_manifest_objects": 1,
                        "catalog_shard_pack_objects": 1,
                        "catalog_shard_max_object_bytes": 1024,
                        "catalog_shard_object_bytes": 3072,
                        "catalog_shard_encode_nanos": 11,
                    },
                    "shard_generate": {
                        "catalog_lazy_shard_bits": 4,
                        "catalog_lazy_root_bytes": 128,
                        "catalog_lazy_map_bytes": 256,
                    },
                    "sharded": {
                        "catalog_lazy_open_median_nanos": 100,
                        "catalog_lazy_open_peak_rss_kib": 1024,
                        "catalog_lazy_open_index_requests": 1,
                        "catalog_lazy_first_lookup_median_nanos": 200,
                        "catalog_lazy_first_lookup_requests": 2,
                "catalog_lazy_first_lookup_bytes": 128 * 1024,
                        "catalog_lazy_cached_lookup_median_nanos": 20,
                        "catalog_lazy_cached_lookup_requests": 0,
                        "catalog_lazy_chunk_list_median_nanos": 300,
                        "catalog_lazy_chunk_list_requests": 4,
                        "catalog_lazy_manifest_list_median_nanos": 400,
                        "catalog_lazy_manifest_list_requests": 4,
                        "catalog_lazy_gc_median_nanos": 500,
                        "catalog_lazy_gc_requests": 4,
                        "catalog_lazy_stream_peak_rss_kib": 2048,
                    },
                    "budget_checks": [],
                }
            ],
        }
        report = catalog.render_report(result)
        self.assertIn("Catalog v1 publication", report)
        self.assertIn("Leveled run sealing", report)
        self.assertIn("Immutable shard layout", report)
        self.assertIn("Measured lazy sharded-base path", report)
        self.assertIn("S3 request amplification projection", report)
        self.assertIn("Production periodic-rebase threshold sweep", report)
        self.assertIn("4.0×", report)
        self.assertIn("| 2 |", report)

    def test_catalog_request_projection_exposes_shard_rewrite_amplification(self):
        projection = catalog.request_amplification_projection(
            storage_tb=500,
            average_chunk_kib=256,
            pack_mib=16,
            run_mib=1,
        )
        self.assertEqual(projection["shard_bits"], 13)
        self.assertEqual(projection["chunks_per_pack"], 64)
        self.assertGreater(projection["delta_packs"], 250)
        self.assertGreater(projection["changed_chunks"], 16_000)
        self.assertGreater(projection["shard_rewrite_gets_per_batch"], 7_000)
        self.assertGreater(projection["request_amplification"], 5_000)
        self.assertGreater(projection["immutable_run_requests_per_batch"], 2)
        self.assertLess(projection["immutable_run_requests_per_batch"], 3)
        self.assertLess(projection["immutable_run_repository_requests"], 350_000)
        self.assertEqual(projection["catalog_pointer_puts"], 0)
        self.assertGreater(
            projection["wal3_embedded_catalog_publications"], 29_000_000
        )
        self.assertEqual(catalog.expected_occupied_prefixes(1, 10), 1)
        self.assertEqual(catalog.expected_occupied_prefixes(8, 0), 0)

    def test_periodic_rebase_projection_exposes_complete_base_cost(self):
        projection = catalog.periodic_rebase_projection(
            storage_tb=500,
            average_chunk_kib=256,
            pack_mib=16,
            run_mib=1,
            rebase_mib=64,
        )
        self.assertEqual(projection["shard_bits"], 13)
        self.assertEqual(projection["chunk_shards"], 8192)
        self.assertEqual(projection["pack_shards"], 8192)
        self.assertEqual(projection["gets_per_rebase"], 16384)
        self.assertEqual(projection["puts_per_rebase"], 16385)
        self.assertEqual(projection["run_object_puts"], projection["run_seals"])
        self.assertEqual(projection["routing_map_puts"], projection["run_seals"])
        self.assertGreater(projection["run_merge_gets"], 0)
        self.assertEqual(
            projection["total_requests"],
            projection["rebase_requests"]
            + projection["run_merge_gets"]
            + projection["run_object_puts"]
            + projection["routing_map_puts"],
        )
        self.assertGreater(projection["rebases"], 4500)
        self.assertGreater(projection["total_requests"], 148_000_000)
        self.assertGreater(projection["requests_per_pack"], 4.9)
        self.assertEqual(projection["max_lookup_run_levels"], 4)
        self.assertLess(projection["open_map_run_routing_bytes"], 300_000)

        sweep = [
            catalog.periodic_rebase_projection(
                storage_tb=500,
                average_chunk_kib=256,
                pack_mib=16,
                run_mib=1,
                rebase_mib=rebase_mib,
            )
            for rebase_mib in (64, 256, 1024, 4096)
        ]
        self.assertEqual([point["max_lookup_run_levels"] for point in sweep], [4, 6, 8, 10])
        self.assertLess(sweep[3]["open_map_run_routing_bytes"], 32 * 1024 * 1024)
        self.assertLess(sweep[1]["total_requests"], sweep[0]["total_requests"] // 3)
        selected = catalog.select_rebase_projection(sweep)
        self.assertEqual(selected["rebase_target_bytes"], 4 * 1024 * 1024 * 1024)


if __name__ == "__main__":
    unittest.main()
