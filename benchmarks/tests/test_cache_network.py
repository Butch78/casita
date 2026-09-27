import copy
import json
import pathlib
import tempfile
import unittest
from unittest import mock

from benchmarks import cli, dashboard
from benchmarks.suites.pack import cache_network as bench


class CacheNetworkTests(unittest.TestCase):
    def config(self):
        return dict(pattern="random", phases=["cold", "warm"], reads=128,
                    cache_bytes=1048576, working_set_bytes=4194304, concurrency=8)

    def samples(self):
        return [dict(status="ok", operation=f"random-{phase}", operations=128,
                     wall_seconds=1, p50_nanos=10, p95_nanos=20, p99_nanos=30, max_nanos=40,
                     backend_read_bytes=4096, pack_range_requests=10, whole_pack_requests=2,
                     cache_hits=3, cache_evictions=4, cache_promotions=2,
                     working_set_bytes=4194304, cache_bytes=1048576, concurrency=8,
                     logical_read_bytes=128*65536, max_in_flight_reads=8,
                     physical_pack_bytes=4200000, fixture_blake3="a"*64, access_order_blake3="b"*64,
                     pack_count=16, pack_target_bytes=262144, file_bytes=65536, correctness="verified")
                for phase in ["cold", "warm"]]

    def output(self, samples):
        return "\n".join("cache_network_sample " + json.dumps(sample) for sample in samples)

    def test_registry_and_axes(self):
        entry = next(entry for entry in cli.entrypoints() if entry["id"] == "pack-cache-network")
        self.assertEqual(entry["target"], "benchmarks.suites.pack.cache_network")
        args = bench.build_parser().parse_args(["--profile", "smoke", "--output", "result.json"])
        self.assertEqual(bench.dimensions(args)[:3], (1, [512, 4096], 128))
        for flags in [["--cache-mib", "0"], ["--working-set-kib", "4096"], ["--reads", "63"],
                      ["--working-set-kib", "513,4096"], ["--rtt-ms", "0,0"]]:
            args = bench.build_parser().parse_args(["--profile", "smoke", "--output", "result.json"] + flags)
            with self.assertRaises(bench.common.BenchmarkError):
                bench.dimensions(args)

    def test_phases_and_invalid_metrics_cannot_pass(self):
        samples = self.samples()
        self.assertEqual(bench.parse_samples(self.output(samples), self.config()), samples)
        for broken in [samples[:1], samples+samples[:1]]:
            with self.assertRaises(bench.common.BenchmarkError):
                bench.parse_samples(self.output(broken), self.config())
        for field, bad in [("wall_seconds", float("nan")), ("p99_nanos", -1),
                           ("cache_hits", True), ("operations", 127), ("concurrency", 1),
                           ("physical_pack_bytes", 1), ("max_in_flight_reads", 9),
                           ("fixture_blake3", "unknown"), ("correctness", "")]:
            broken = copy.deepcopy(samples)
            broken[0][field] = bad
            with self.subTest(field=field), self.assertRaises(bench.common.BenchmarkError):
                bench.parse_samples(self.output(broken), self.config())

    def test_fitting_warm_cache_must_not_fetch(self):
        config = dict(self.config(), working_set_bytes=524288)
        samples = self.samples()
        for sample in samples:
            sample.update(working_set_bytes=524288, physical_pack_bytes=525000)
        with self.assertRaisesRegex(bench.common.BenchmarkError, "fitting warm"):
            bench.parse_samples(self.output(samples), config)
        samples[1].update(pack_range_requests=0, whole_pack_requests=0, backend_read_bytes=0)
        bench.parse_samples(self.output(samples), config)

    def test_cold_label_requires_a_backend_fetch(self):
        samples = self.samples()
        samples[0].update(pack_range_requests=0, whole_pack_requests=0, backend_read_bytes=0)
        with self.assertRaisesRegex(bench.common.BenchmarkError, "fresh cold"):
            bench.parse_samples(self.output(samples), self.config())

    def test_pairs_require_identical_data_layout_and_order(self):
        before = self.samples()
        bench.verify_pair(before, list(reversed(before)))
        for field, bad in [("fixture_blake3", "c"*64), ("access_order_blake3", "c"*64),
                           ("pack_count", 17), ("physical_pack_bytes", 4300000)]:
            after = copy.deepcopy(before)
            after[0][field] = bad
            with self.subTest(field=field), self.assertRaises(bench.common.BenchmarkError):
                bench.verify_pair(before, after)

    def test_failure_retains_partial_results(self):
        with tempfile.TemporaryDirectory() as tmp:
            output = pathlib.Path(tmp)/"result.json"
            def failed(args, work, result):
                result["processes"].append({"stderr": "failure evidence"})
                raise bench.common.BenchmarkError("probe failed")
            with mock.patch.object(bench, "run", side_effect=failed), mock.patch.object(bench.common, "environment_metadata", return_value={}):
                with self.assertRaisesRegex(bench.common.BenchmarkError, "probe failed"):
                    bench.main(["--output", str(output)])
            result = json.loads(output.read_text())
            self.assertFalse(result["complete"])
            self.assertEqual(result["processes"][0]["stderr"], "failure evidence")
            self.assertEqual(result["error"], "probe failed")

    def test_dashboard_keeps_variants_and_concurrency_separate(self):
        samples = [dict(self.samples()[0], variant=variant, concurrency=concurrency)
                   for variant in ["before", "after"] for concurrency in [1, 8]]
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp)/"result.json"
            path.write_text(json.dumps(dict(result_schema="casita.scale.v1", suite_id="huge-repositories", complete=True,
                                           samples=samples, configuration={})))
            self.assertEqual(len(dashboard.normalize_result(path)["observations"]), 4)
