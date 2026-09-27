import contextlib
import io
import json
import pathlib
import tempfile
import unittest

from benchmarks import comparison, dashboard, metrics


ROOT = pathlib.Path(__file__).parents[2]
MANIFEST = ROOT / "benchmarks" / "manifest.json"


def run(revision: str, wall: float, throughput: float, *, status: str = "ok") -> dict:
    return {
        "suite_id": "repository-e2e",
        "environment": {"casita_revision": revision},
        "observations": [
            {
                "workload": "small/files",
                "profile": "pr",
                "cache_policy": "warm",
                "operation": "checkout",
                "implementation": "casita",
                "status": status,
                "metrics": {
                    "wall_seconds": wall,
                    "throughput_bytes_per_second": throughput,
                    "unregistered_diagnostic": 99,
                },
            }
        ],
    }


class MetricRegistryTests(unittest.TestCase):
    def setUp(self):
        self.manifest = dashboard.load_manifest(MANIFEST)
        self.registry = metrics.load_metric_registry(self.manifest)

    def test_bmf_uses_stable_short_identity_and_registered_measures(self):
        document = metrics.bmf_document([run("head", 1.5, 100)], self.registry)
        self.assertEqual(len(document), 1)
        name, measures = next(iter(document.items()))
        self.assertLessEqual(len(name), 64)
        self.assertRegex(name, r"^repository-e2e-checkout-casita-[0-9a-f]{12}$")
        self.assertNotIn("head", name)
        self.assertEqual(measures["wall-seconds"], {"value": 1.5})
        self.assertEqual(measures["throughput-bytes-per-second"], {"value": 100.0})
        self.assertNotIn("unregistered-diagnostic", measures)

    def test_bencher_identity_hash_distinguishes_shared_prefixes(self):
        first = metrics.observation_identity(run("head", 1, 1), run("head", 1, 1)["observations"][0])
        other_run = run("head", 1, 1)
        other_run["observations"][0]["workload"] = "different"
        second = metrics.observation_identity(other_run, other_run["observations"][0])
        self.assertNotEqual(
            metrics.bencher_benchmark_name(first), metrics.bencher_benchmark_name(second)
        )

    def test_failed_observations_are_not_exported(self):
        self.assertEqual(metrics.bmf_document([run("head", 1, 1, status="failed")], self.registry), {})

    def test_duplicate_observation_is_rejected(self):
        with self.assertRaisesRegex(metrics.MetricRegistryError, "duplicate benchmark identity"):
            metrics.bmf_document([run("head", 1, 1), run("head", 1, 1)], self.registry)

    def test_registry_rejects_gated_neutral_metric(self):
        manifest = {"metrics": [{
            "id": "requests",
            "measure": "requests",
            "unit": "requests",
            "direction": "neutral",
            "comparison": "exact",
            "gate": True,
        }]}
        with self.assertRaisesRegex(metrics.MetricRegistryError, "needs a direction"):
            metrics.load_metric_registry(manifest)


class ComparisonTests(unittest.TestCase):
    def setUp(self):
        self.registry = metrics.load_metric_registry(dashboard.load_manifest(MANIFEST))

    def test_comparison_reports_direction_without_claiming_threshold_regressions(self):
        result = comparison.compare_runs(
            [run("base", 2.0, 100)],
            [run("head", 1.5, 80)],
            self.registry,
        )
        rows = {row["metric"]: row for row in result["comparisons"]}
        self.assertEqual(rows["wall_seconds"]["outcome"], "decreased")
        self.assertEqual(rows["wall_seconds"]["change_percent"], -25.0)
        self.assertEqual(rows["throughput_bytes_per_second"]["outcome"], "decreased")
        self.assertEqual(rows["wall_seconds"]["direction"], "lower")
        self.assertEqual(rows["throughput_bytes_per_second"]["direction"], "higher")
        self.assertEqual(result["base_revision"], "base")
        self.assertEqual(result["head_revision"], "head")
        self.assertEqual(result["summary"]["decreased"], 2)

    def test_result_sets_must_contain_one_revision(self):
        with self.assertRaisesRegex(comparison.ComparisonError, "multiple revisions"):
            comparison.revision_of([run("a", 1, 1), run("b", 1, 1)])

    def test_failed_pair_is_not_performance_compared(self):
        result = comparison.compare_runs(
            [run("base", 2.0, 100)],
            [run("head", 0.1, 1000, status="failed")],
            self.registry,
        )
        self.assertEqual(result["comparisons"], [])
        self.assertEqual(result["skipped_failed_observations"], 1)

    def test_interleaved_rounds_coalesce_to_one_revision(self):
        result = comparison.coalesce_repeated_runs(
            [run("revision", 2.0, 100), run("revision", 4.0, 300)],
            self.registry,
        )
        observation = result["observations"][0]
        self.assertEqual(result["environment"]["casita_revision"], "revision")
        self.assertEqual(observation["rounds"], 2)
        self.assertEqual(observation["samples"], 2)
        self.assertEqual(observation["metrics"]["wall_seconds"], 3.0)
        self.assertEqual(observation["metrics"]["throughput_bytes_per_second"], 200.0)

    def test_revision_series_tracks_baseline_and_previous_changes(self):
        result = comparison.revision_series(
            [
                ("a", [run("aaa", 4.0, 100)]),
                ("b", [run("bbb", 3.0, 120)]),
                ("c", [run("ccc", 2.0, 90)]),
            ],
            self.registry,
            "a",
        )
        wall = next(row for row in result["rows"] if row["metric"] == "wall_seconds")
        self.assertEqual(result["baseline_label"], "a")
        self.assertEqual([entry["label"] for entry in result["revisions"]], ["a", "b", "c"])
        self.assertEqual(wall["values"][1]["change_from_baseline_percent"], -25.0)
        self.assertEqual(wall["values"][2]["change_from_baseline_percent"], -50.0)
        self.assertAlmostEqual(wall["values"][2]["change_from_previous_percent"], -100 / 3)
        report = comparison.render_series_report(result)
        self.assertIn("| Benchmark | Metric | a | b | c |", report)
        self.assertIn("(-50.00%)", report)
        self.assertIn("| Benchmark | Metric | a → b | b → c |", report)
        self.assertIn("-33.33% (decreased)", report)

    def test_cli_exports_bmf_to_stdout(self):
        raw = {
            "suite_id": "repository-e2e",
            "result_schema": "casita.repository-e2e.v1",
            "environment": {"casita_revision": "head", "casita_worktree_dirty": False},
            "configuration": {"profile": "pr"},
            "corpora": {"small-files": {"base_bytes": 100}},
            "samples": [],
            "aggregates": [{
                "corpus": "small-files",
                "cache_policy": "warm",
                "operation": "checkout",
                "implementation": "casita",
                "samples": 3,
                "median_wall_seconds": 1.5,
                "p95_wall_seconds": 2.0,
                "median_max_rss_bytes": 1024,
                "median_throughput_bytes_per_second": 100,
                "median_repository_allocated_bytes": 2048,
                "median_storage_metrics": {},
            }],
        }
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "head.json"
            path.write_text(json.dumps(raw))
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                exit_code = comparison.main(["export-bencher", "--result", str(path)])
        self.assertEqual(exit_code, 0)
        document = json.loads(output.getvalue())
        self.assertEqual(next(iter(document.values()))["wall-seconds"]["value"], 1.5)


if __name__ == "__main__":
    unittest.main()
