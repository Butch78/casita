import unittest

from benchmarks.suites import output_import


class OutputImportTests(unittest.TestCase):
    def test_parse_sample_requires_exact_configuration_and_correctness(self):
        stdout = (
            "running 1 test\n"
            "test scale_benchmarks::benchmark_output_import ... ok\n"
            'output_import_sample {"operation":"output-import","mode":"batched",'
            '"outputs":8,"output_bytes":131072,"logical_bytes":1048576,"nanos":10,'
            '"session_nanos":1,"stage_nanos":2,"publish_nanos":3,'
            '"correctness":"exact roots, byte-for-byte payload reads, clean fsck"}\n'
            "test result: ok. 1 passed; 0 failed;\n"
        )
        sample = output_import.parse_sample(stdout, 8, 131072, "batched")
        self.assertEqual(sample["logical_bytes"], 1048576)

    def test_parse_sample_rejects_missing_correctness_gate(self):
        stdout = (
            "test result: ok. 1 passed; 0 failed;\n"
            'output_import_sample {"operation":"output-import","mode":"per-output",'
            '"outputs":1,"output_bytes":6,"logical_bytes":6,"nanos":10,'
            '"session_nanos":1,"stage_nanos":2,"publish_nanos":3}\n'
        )
        with self.assertRaisesRegex(RuntimeError, "correctness"):
            output_import.parse_sample(stdout, 1, 6, "per-output")
