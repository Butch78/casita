import json
import pathlib
import unittest

from benchmarks.suites import gix_odb as benchmark


class GixOdbBenchmarkTests(unittest.TestCase):
    def test_sample_parser_keeps_pack_counters(self):
        sample = {
            "implementation": "casita",
            "backend": "local",
            "operation": "warm-find",
            "operations": 4,
            "wall_nanos": 100,
            "nanos_per_op": 25,
            "status": "ok",
            "pack": {"cache_hits": 4},
        }
        parsed = benchmark.parse_samples(
            "ignored\n" + benchmark.SAMPLE_PREFIX + json.dumps(sample) + "\n"
        )
        self.assertEqual(parsed[0]["pack"]["cache_hits"], 4)

    def test_benchmark_artifact_parser_finds_the_bench_binary(self):
        artifact = {
            "reason": "compiler-artifact",
            "target": {"name": "gix_odb", "kind": ["bench"]},
            "executable": "/tmp/target/release/deps/gix_odb-1234",
        }
        self.assertEqual(
            benchmark.parse_benchmark_binary(json.dumps(artifact)),
            pathlib.Path("/tmp/target/release/deps/gix_odb-1234"),
        )

    def test_report_distinguishes_backend_and_implementation(self):
        result = {
            "configuration": {
                "profile": "smoke",
                "repetitions": 1,
                "scale": {"objects": 4, "body_bytes": 8},
            },
            "samples": [
                {
                    "implementation": "casita",
                    "backend": "local",
                    "operation": "cold-find",
                    "repetition": 1,
                    "nanos_per_op": 10,
                    "throughput_bytes_per_second": 32,
                    "pack": {"chunk_range_requests": 4, "cache_hits": 0},
                }
            ],
        }
        report = benchmark.render_report(result)
        self.assertIn("| casita | local | cold-find |", report)
        self.assertIn("| 4 | 0 |", report)


if __name__ == "__main__":
    unittest.main()
