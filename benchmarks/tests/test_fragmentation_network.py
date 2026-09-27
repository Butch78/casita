import json
import pathlib
import tempfile
import unittest
from unittest import mock

from benchmarks import all as runner, cli
from benchmarks.suites.pack import fragmentation_network as bench


class FragmentationNetworkTests(unittest.TestCase):
    def fixture(self):
        descriptor = dict(file_bytes=8 * 2**20, chunks=32, referenced_packs=3, pack_runs=5,
            largest_pack_bytes=2**20, referenced_pack_bytes=3 * 2**20, stored_bytes=4 * 2**20,
            stored_pack_bytes=3 * 2**20, blob="same-content")
        return dict(history=dict(descriptor, prefix="fixture/history"), fresh=dict(descriptor, prefix="fixture/fresh"),
            generations=4, pack_target_bytes=16 * 2**20, avg_chunk_bytes=256 * 1024,
            default_cache_bytes=64 * 2**20, original_blake3="a" * 64,
            writes=[dict(generation=n) for n in range(5)], correctness=bench.PREPARED)

    def counters(self, gets=0, size=0):
        return dict(gets=gets, get_bytes=size, heads=0, puts=0, put_bytes=0, lists=0)

    def physical_fixture(self):
        fixture = self.fixture()
        for layout in ("history", "fresh"):
            fixture[layout]["read_plan"] = [dict(digest=str(i), size=256*1024,
                pack=str(i % 3), offset=(i // 3)*8, framed_len=8, pack_len=2**20)
                for i in range(32)]
            fixture[layout]["pack_runs"] = 32
        return fixture

    def test_physical_trace_requires_matching_logical_order(self):
        fixture = self.physical_fixture()
        config = dict(generations=4, prefix="fixture")
        bench.parse_prepared(self.output([fixture], "s3_fragmentation_prepared "), config)
        fixture["fresh"]["read_plan"].reverse()
        with self.assertRaises(bench.common.BenchmarkError):
            bench.parse_prepared(self.output([fixture], "s3_fragmentation_prepared "), config)

    def config(self):
        return dict(prepared=self.fixture(), layout="history", cache="ample", cache_bytes=3 * 2**20)

    def samples(self):
        return [dict(phase=phase, layout="history", cache="ample", cache_bytes=3 * 2**20,
                     blob="same-content", file_bytes=8 * 2**20, nanos=1, reopen_nanos=1,
                     pack_requests=3 if phase == "cold" else 0, pack_read_bytes=400 if phase == "cold" else 0,
                     whole_pack_requests=3 if phase == "cold" else 0, chunk_range_requests=0,
                     cache_hits=1, cache_promotions=0, cache_evictions=0,
                     origin=self.counters(4 if phase == "cold" else 1, 500 if phase == "cold" else 100),
                     reopen_origin=self.counters(1, 100), correctness=bench.CORRECTNESS)
                for phase in ("cold", "warm")]

    def output(self, samples, prefix="s3_fragmentation_sample "):
        return "\n".join(prefix + json.dumps(s) for s in samples) + "\ntest result: ok. 1 passed; 0 failed;"

    def test_registry_and_build_contract(self):
        self.assertIn("s3-fragmentation", {e["id"] for e in cli.entrypoints()})
        self.assertEqual(runner.suite_arguments("s3-fragmentation", pathlib.Path("/bin"), "smoke", 1),
            ["--profile", "smoke", "--read-bytes", "0,1", "--include-small-buffer-control", "--probe-binary", "/bin/casita-lib-test", "--no-build", "--repetitions", "1"])

    def test_cache_capacity_is_shared_and_covers_boundary(self):
        fixture = self.fixture()
        fixture["fresh"]["largest_pack_bytes"] += 10
        self.assertEqual([bench.cache_capacity(fixture, c) for c in bench.CACHES[:6]],
                         [0, 2**20 + 9, 2**20 + 11, 64 * 2**20, 3 * 2**20 - 1, 3 * 2**20])

    def test_live_cache_boundary_counts_unique_compressed_chunks(self):
        fixture = self.fixture()
        for layout in ("fresh", "history"):
            fixture[layout]["read_plan"] = [dict(digest="a", framed_len=40), dict(digest="b", framed_len=60), dict(digest="a", framed_len=40)]
        self.assertEqual([bench.cache_capacity(fixture, c) for c in bench.CACHES[6:]], [99, 100, 101])

    def test_all_standard_keeps_proxy_control(self):
        self.assertIn("--include-small-buffer-control",
                      runner.suite_arguments("s3-fragmentation", pathlib.Path("/bin"), "standard", 1))

    def test_focused_cache_selection_keeps_full_matrix_by_default(self):
        self.assertEqual(bench.build_parser().parse_args([]).caches, list(bench.CACHES))
        self.assertEqual(bench.build_parser().parse_args(["--caches", "default", "ample"]).caches,
                         ["default", "ample"])
        with self.assertRaises(bench.common.BenchmarkError):
            bench.main(["--caches", "ample", "ample"])

    def test_invalid_proxy_controls_fail_before_build(self):
        for flags in (("--proxy-buffer-kib", "0"), ("--proxy-buffer-kib", "65"),
                      ("--include-small-buffer-control", "--rtt-ms", "0"),
                      ("--include-small-buffer-control", "--proxy-buffer-kib", "512")):
            with self.assertRaises(bench.common.BenchmarkError):
                bench.main(list(flags))

    def test_prepare_requires_production_defaults_and_history(self):
        config = dict(prefix="fixture", generations=4)
        fixture = self.fixture()
        bench.parse_prepared(self.output([fixture], "s3_fragmentation_prepared "), config)
        for changed in (dict(fixture, generations=3), dict(fixture, pack_target_bytes=2**20),
                        dict(fixture, writes=fixture["writes"][:-1]),
                        dict(fixture, fresh=dict(fixture["fresh"], blob="different"))):
            with self.assertRaises(bench.common.BenchmarkError):
                bench.parse_prepared(self.output([changed], "s3_fragmentation_prepared "), config)

    def test_valid_matrix_and_missing_phases(self):
        bench.parse_samples(self.output(self.samples()), self.config())
        for output in (self.output(self.samples()[:1]), self.output(self.samples() + self.samples()[:1]),
                       self.output(self.samples()).replace("1 passed", "0 passed")):
            with self.assertRaises(bench.common.BenchmarkError):
                bench.parse_samples(output, self.config())

    def test_origin_accounting_and_read_only_gates(self):
        for field, value in (("gets", 2), ("get_bytes", 1), ("puts", 1), ("put_bytes", 1), ("heads", True)):
            samples = self.samples()
            samples[0]["origin"][field] = value
            with self.subTest(field=field), self.assertRaises(bench.common.BenchmarkError):
                bench.parse_samples(self.output(samples), self.config())

    def test_per_pack_trace_matches_totals_and_rejects_duplicates(self):
        samples = self.samples()
        pack = dict(path="history/packs/id", gets=3, bytes=400, whole_gets=3)
        samples[0]["pack_io"] = [pack]
        samples[1]["pack_io"] = []
        bench.parse_samples(self.output(samples), self.config())
        for trace in (None, [pack, pack], [dict(pack, bytes=399)],
                      [dict(pack, whole_gets=4)], [dict(pack, path="metadata/id")]):
            samples[0]["pack_io"] = trace
            with self.assertRaises(bench.common.BenchmarkError):
                bench.parse_samples(self.output(samples), self.config())

    def test_warm_cold_identity_and_metrics_gates(self):
        for index, field, value in ((0, "nanos", 0), (0, "nanos", float("nan")),
                (0, "cache_bytes", 0), (0, "blob", "different"), (0, "correctness", "unchecked"),
                (0, "pack_requests", 0), (1, "pack_requests", 1)):
            samples = self.samples()
            samples[index][field] = value
            with self.subTest(field=field), self.assertRaises(bench.common.BenchmarkError):
                bench.parse_samples(self.output(samples), self.config())

    def test_production_live_cache_gate_uses_compact_count_without_trace(self):
        config = dict(self.config(), live_chunk_bytes=3 * 2**20 - 100, cache_bytes=3 * 2**20 - 100)
        samples = self.samples()
        for sample in samples:
            sample.update(cache_bytes=config["cache_bytes"], production_fetch="planned-chunk-cache-v1")
        bench.parse_samples(self.output(samples), config)
        samples[1].update(pack_requests=1, chunk_range_requests=1, pack_read_bytes=1)
        with self.assertRaisesRegex(bench.common.BenchmarkError, "fitting compressed-chunk"):
            bench.parse_samples(self.output(samples), config)

    def test_failure_retains_process_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory) / "result.json"
            def fail(args, work, result):
                result["processes"].append(dict(stderr="failure details"))
                raise bench.common.BenchmarkError("probe failed")
            with mock.patch.object(bench, "run", side_effect=fail), \
                 mock.patch.object(bench.common, "environment_metadata", return_value={}):
                with self.assertRaises(bench.common.BenchmarkError):
                    bench.main(["--output", str(output)])
            result = json.loads(output.read_text())
            self.assertFalse(result["complete"])
            self.assertEqual(result["processes"][0]["stderr"], "failure details")

    def test_paired_run_shares_fixture_and_retains_variant_provenance(self):
        self.check_paired_run(["history", "fresh"])

    def test_paired_fresh_control_retains_both_phases_and_variants(self):
        self.check_paired_run(["fresh"])

    def test_same_binary_planned_short_read_retains_strategy_and_timing(self):
        self.check_paired_run(["fresh"], planned=True)

    def test_lookahead_run_retains_same_binary_pipeline_control(self):
        self.check_paired_run(["history", "fresh"], planned=True, control=True)

    def check_paired_run(self, layouts, planned=False, control=False):
        with tempfile.TemporaryDirectory() as directory:
            work = pathlib.Path(directory)
            args = bench.build_parser().parse_args(["--profile", "smoke", "--repetitions", "1",
                "--rtt-ms", "0", "--caches", "ample", "--probe-binary", "/candidate",
                "--baseline-probe-binary", "/baseline", "--output", str(work / "result.json")])
            args.layouts = layouts
            if planned:
                args.compare_current = True
                args.baseline_probe_binary = None
                args.candidate_read_strategy = "lookahead" if control else "planned"
                args.include_pipeline_control = control
                args.read_bytes = "1"
            result = dict(configuration={}, fixtures={}, processes=[], samples=[], calibration=[], complete=False)
            log = work / "server.log"
            log.write_text("")
            server = mock.Mock(endpoint="http://localhost:1234", port=1234, log_path=log)
            response = mock.MagicMock()
            response.__enter__.return_value.status = 200
            proxy = mock.MagicMock()
            proxy.__enter__.return_value.endpoint = "http://localhost:4321"

            def invoke(binary, config, work, result):
                result["processes"].append(dict(binary=str(binary), config=config))
                if config["mode"] == "prepare":
                    fixture = self.physical_fixture()
                    for layout in ("history", "fresh"):
                        fixture[layout]["prefix"] = config["prefix"] + "/" + layout
                    return self.output([fixture], "s3_fragmentation_prepared ")
                self.assertTrue(all("read_plan" not in d for d in config["prepared"].values()))
                samples = self.samples()
                for sample in samples:
                    sample["layout"] = config["layout"]
                    if planned:
                        sample.update(first_byte_nanos=1, read_bytes=1, read_strategy=config["read_strategy"], response_peak_bytes=400)
                return self.output(samples)

            with mock.patch.object(bench, "fingerprint", return_value="hash"), \
                 mock.patch.object(bench.subprocess, "check_output", return_value="rustfs"), \
                 mock.patch.object(bench, "Rustfs", return_value=server), \
                 mock.patch.object(bench, "create_rustfs_bucket"), \
                 mock.patch.object(bench, "TcpLatencyProxy", return_value=proxy), \
                 mock.patch.object(bench.urllib.request, "urlopen", return_value=response), \
                 mock.patch.object(bench, "invoke", side_effect=invoke):
                bench.run(args, work, result)
            variants = ["candidate", "baseline", "pipeline-control"] if control else ["candidate", "baseline"]
            expected = 2 * len(variants) * len(layouts)
            self.assertEqual(result["expected_samples"], expected)
            self.assertEqual(len(result["samples"]), expected)
            reads = [p for p in result["processes"] if p["config"]["mode"] == "read"]
            self.assertEqual([p["config"]["variant"] for p in reads],
                             variants * len(layouts))
            self.assertTrue(all(p["binary"] == ("/candidate" if planned else "/" + p["config"]["variant"]) for p in reads))
            self.assertEqual(len([p for p in result["processes"] if p["config"]["mode"] == "prepare"]), 1)
            self.assertEqual(len({(s["variant"], s["layout"], s["phase"]) for s in result["samples"]}), expected)
            self.assertEqual({s["layout"] for s in result["samples"]}, set(layouts))

    def test_stream_metrics_reject_wrong_strategy_and_peak(self):
        config = dict(self.config(), read_strategy="planned", read_bytes=1, require_stream_metrics=True)
        for field, value in (("first_byte_nanos", 2), ("read_strategy", "current"),
                             ("read_bytes", 0), ("response_peak_bytes", 64*2**20+1)):
            samples = self.samples()
            for sample in samples:
                sample.update(first_byte_nanos=1, read_strategy="planned", read_bytes=1, response_peak_bytes=400)
            samples[0][field] = value
            with self.subTest(field=field), self.assertRaises(bench.common.BenchmarkError):
                bench.parse_samples(self.output(samples), config)

    def test_pipeline_registry_and_response_gate(self):
        entry = next(e for e in cli.entrypoints() if e["id"] == "s3-fetch-pipeline")
        self.assertIn("pipeline", entry["default_arguments"])
        self.assertIn("--no-build", runner.suite_arguments("s3-fetch-pipeline", pathlib.Path("/bin"), "smoke", 1))
        config = dict(self.config(), read_strategy="pipeline", read_bytes=1, require_stream_metrics=True)
        samples = self.samples()
        for sample in samples:
            sample.update(first_byte_nanos=1, read_strategy="pipeline", read_bytes=1, response_peak_bytes=0)
        bench.parse_samples(self.output(samples), config)
        samples[0]["response_peak_bytes"] = 64*2**20+1
        with self.assertRaises(bench.common.BenchmarkError):
            bench.parse_samples(self.output(samples), config)

    def test_cancelled_partial_read_preserves_distinct_io_ledgers(self):
        samples = self.samples()
        samples[0]["pack_io"] = [dict(path="history/packs/id", gets=2, bytes=450, whole_gets=2)]
        samples[1].update(pack_requests=1, chunk_range_requests=1, pack_io=[])
        config = dict(self.config(), read_bytes=1)
        bench.parse_samples(self.output(samples), config)
        with self.assertRaises(bench.common.BenchmarkError):
            bench.parse_samples(self.output(samples), self.config())
        for field, value in (("gets", 4), ("bytes", 399), ("bytes", 501), ("whole_gets", 3)):
            changed = json.loads(json.dumps(samples))
            changed[0]["pack_io"][0][field] = value
            with self.subTest(field=field, value=value), self.assertRaises(bench.common.BenchmarkError):
                bench.parse_samples(self.output(changed), config)
