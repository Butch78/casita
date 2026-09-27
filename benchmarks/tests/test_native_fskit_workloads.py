import gzip
from pathlib import Path
import time
import tempfile
import unittest
from unittest import mock

from benchmarks.suites import native_fskit_workloads as suite
from benchmarks.suites.repository import BenchmarkError


class WorkloadTests(unittest.TestCase):
    def test_fixture_rejects_wrappers_that_could_execute_host_binaries(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            wrapper = root / 'wrapper'
            wrapper.write_bytes(b'#!/bin/sh\nexec /host/tool "$@"\n')
            with mock.patch.dict(suite.os.environ, {'CASITA_WORKLOAD_AWK':str(wrapper)}):
                with self.assertRaisesRegex(BenchmarkError, 'underlying executable'):
                    suite.add_fixture(root, {})

    def test_tool_oracles_reject_wrong_results(self):
        for tool, output in (("awk", suite.NUMBERED), ("sort", suite.SORTED), ("gzip", gzip.compress(suite.PAYLOAD))):
            suite.check_output(tool, output)
            with self.assertRaises((BenchmarkError, OSError, EOFError)):
                suite.check_output(tool, b"wrong")

    def test_batch_uses_one_or_distinct_paths_and_checks_every_worker(self):
        def execute(path, tool, gate):
            gate.wait(timeout=5)
            return {"path":str(path), "tool":tool, "finish_ns":time.perf_counter_ns(), "correctness":"passed"}
        with mock.patch.object(suite, "execute", side_effect=execute):
            for pattern, unique in (("shared", 1), ("distinct", 3)):
                result = suite.batch(Path('/fixture'), pattern, 3)
                self.assertEqual(len(result['samples']), 3)
                self.assertEqual(len({s['path'] for s in result['samples']}), unique)
                self.assertGreater(result['wall_ns'], 0)

    def test_matrix_rejects_missing_workers_or_cleanup(self):
        rows = [dict(repetition=0, pattern=p, workers=n, implementation=b, tree=f'/{p}/{n}/{b}',
                     correctness='passed', teardown='passed',
                     first={'samples':[{'correctness':'passed'} for _ in range(n)]},
                     repeat={'samples':[{'correctness':'passed'} for _ in range(n)]})
                for p in suite.PATTERNS for n in suite.COUNTS for b in ('native','host')]
        suite.validate(rows, 1, ('native','host'))
        for invalid in (rows[:-1], rows + [rows[0]],
                        [{**rows[0], 'repeat':{'samples':[]}}, *rows[1:]],
                        [{**rows[0], 'teardown':'pending'}, *rows[1:]]):
            with self.assertRaises(BenchmarkError):
                suite.validate(invalid, 1, ('native','host'))


if __name__ == '__main__':
    unittest.main()
