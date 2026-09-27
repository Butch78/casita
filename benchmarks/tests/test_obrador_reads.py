import json
import hashlib
import pathlib
import subprocess
import tempfile
import unittest
from unittest import mock

from benchmarks import all as corpus
from benchmarks.suites import obrador_reads as suite
from benchmarks.suites.repository import BenchmarkError


class ObradorReadTests(unittest.TestCase):
    def test_contaminated_timeout_is_retained_and_retried(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            binary = root / 'binary'
            binary.write_bytes(b'fixture')
            previous = root / 'previous.json'
            previous.write_text(json.dumps({
                'result_schema': 'casita.obrador-reads.v1', 'complete': True,
                'environment': {}, 'sources': {},
                'binaries': {variant: {'path': str(binary),
                    'sha256': hashlib.sha256(binary.read_bytes()).hexdigest()}
                    for variant in ('durable', 'process')}}))
            reports = iter([False, True, True, True, True])
            def monitor(**kwargs):
                host = mock.MagicMock()
                host.__enter__.return_value = host
                host.report.return_value = {'quiet': next(reports)}
                return host
            calls = 0
            def run(command, **kwargs):
                nonlocal calls
                calls += 1
                if calls == 1:
                    raise subprocess.TimeoutExpired(command, 300)
                gc = command[-1] == 'true'
                kwargs['stdout'].write(json.dumps(dict(paths=1, workers=1, iterations=2,
                    concurrent_gc=gc, correctness=suite.CORRECTNESS, read_nanos=[10, 10],
                    gc_nanos=[30] if gc else [], p50_nanos=10, p95_nanos=10,
                    p99_nanos=10, wall_seconds=0.1)))
            output = root / 'result.json'
            with mock.patch.object(suite.common, 'environment_metadata', return_value={}), \
                 mock.patch('benchmarks.host_activity.QuietHost', side_effect=monitor), \
                 mock.patch.object(suite.subprocess, 'run', side_effect=run):
                self.assertEqual(suite.main(['--reuse-build-report', str(previous),
                    '--output', str(output), '--paths', '1', '--workers', '1',
                    '--iterations', '2', '--require-quiet-host']), 0)
            result = json.loads(output.read_text())
            self.assertTrue(result['complete'])
            self.assertEqual(len(result['samples']), 4)
            self.assertEqual(len(result['failed_attempts']), 1)
            self.assertFalse(result['failed_attempts'][0]['host_activity']['quiet'])
            self.assertTrue(all(row['host_activity']['quiet'] for row in result['samples']))

    def test_all_forwards_external_source(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            binaries = root / "bin"
            binaries.mkdir()
            def execute(command, *args):
                self.assertIn("obrador-reads", command)
                self.assertEqual(command[command.index("--obrador-source") + 1], str(root.resolve()))
                return {"status": "passed", "exit_code": 0}
            with mock.patch.object(corpus, "execute", side_effect=execute), \
                 mock.patch.object(corpus.common, "environment_metadata", return_value={}):
                self.assertEqual(corpus.main([
                    "--suites", "obrador-reads", "--bin-dir", str(binaries),
                    "--obrador-source", str(root), "--output", str(root / "results")]), 0)

    def test_reuse_refuses_changed_binary_and_records_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            binary = root / "probe"
            binary.write_bytes(b"changed")
            previous = root / "previous.json"
            previous.write_text(json.dumps({
                "result_schema": "casita.obrador-reads.v1", "complete": True,
                "binaries": {variant: {"path": str(binary), "sha256": "incorrect"}
                             for variant in ("durable", "process")}}))
            output = root / "next.json"
            with mock.patch.object(suite.common, "environment_metadata", return_value={}), \
                 self.assertRaisesRegex(BenchmarkError, "binary changed"):
                suite.main(["--reuse-build-report", str(previous), "--output", str(output)])
            result = json.loads(output.read_text())
            self.assertFalse(result["complete"])
            self.assertEqual(result["samples"], [])
            self.assertIn("binary changed", result["error"])

    def test_requires_complete_verified_reads_and_gc(self):
        case = dict(paths=12, workers=1, iterations=2, concurrent_gc=True,
                    correctness=suite.CORRECTNESS, read_nanos=[10, 20],
                    gc_nanos=[30], p50_nanos=10, p95_nanos=20, p99_nanos=20,
                    wall_seconds=0.1)
        def parse(row):
            return suite.parse_sample(json.dumps(row), 12, 1, 2, True)
        self.assertEqual(parse(case), case)
        for change in ({"read_nanos": [10]}, {"gc_nanos": []},
                       {"correctness": "unchecked"}, {"p95_nanos": 10},
                       {"workers": 8}, {"read_nanos": [10, -1]}):
            with self.subTest(change=change), self.assertRaises(BenchmarkError):
                parse({**case, **change})

    def test_rejects_collection_in_gc_off_case(self):
        case = dict(paths=12, workers=1, iterations=1, concurrent_gc=False,
                    correctness=suite.CORRECTNESS, read_nanos=[10], gc_nanos=[],
                    p50_nanos=10, p95_nanos=10, p99_nanos=10, wall_seconds=0.1)
        self.assertEqual(suite.parse_sample(json.dumps(case), 12, 1, 1, False), case)
        case["gc_nanos"] = [30]
        with self.assertRaises(BenchmarkError):
            suite.parse_sample(json.dumps(case), 12, 1, 1, False)
