import json
import unittest

from benchmarks.suites.catalog_marking import CORRECTNESS, parse_sample
from benchmarks.suites.repository import BenchmarkError


class CatalogMarkingTest(unittest.TestCase):
    def output(self, **changes):
        row = dict(count=1024, holds=8, mode="overlap", paths=4096,
                   nanos=100, path_bytes=1000, index_requests=33, index_bytes=100,
                   rss_before=1024, rss_after=2048, correctness=CORRECTNESS)
        row.update(changes)
        return 'catalog_marking_sample ' + json.dumps(row) + '\ntest result: ok. 1 passed; 0 failed;'

    def test_accepts_overlap(self):
        parse_sample(self.output(), 1024, 8, 'overlap')

    def test_disjoint_requires_full_union(self):
        with self.assertRaises(BenchmarkError):
            parse_sample(self.output(mode='disjoint'), 1024, 8, 'disjoint')
        parse_sample(self.output(mode='disjoint', paths=32768), 1024, 8, 'disjoint')

    def test_rejects_wrong_config_or_metrics(self):
        for changes in (dict(holds=1), dict(nanos=0), dict(rss_after=-1), dict(correctness='ok')):
            with self.subTest(changes=changes), self.assertRaises(BenchmarkError):
                parse_sample(self.output(**changes), 1024, 8, 'overlap')

    def test_requires_one_completed_probe(self):
        output = self.output()
        for invalid in (output.split('\n')[0], output + '\n' + output):
            with self.assertRaises(BenchmarkError):
                parse_sample(invalid, 1024, 8, 'overlap')


class CatalogMarkingPairTest(unittest.TestCase):
    def test_pairs_binaries_and_alternates_order(self):
        import pathlib
        import subprocess
        import tempfile
        from unittest import mock
        from benchmarks.suites import catalog_marking

        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            before, after, output = root / 'before', root / 'after', root / 'result.json'
            before.write_bytes(b'baseline')
            after.write_bytes(b'candidate')
            calls = []

            def run(command, **kwargs):
                calls.append(pathlib.Path(command[0]).name)
                env = kwargs['env']
                row = dict(count=int(env['CASITA_BENCH_MARK_ENTRIES']),
                           holds=int(env['CASITA_BENCH_MARK_HOLDS']),
                           mode=env['CASITA_BENCH_MARK_MODE'], paths=4096,
                           nanos=100, path_bytes=1000, index_requests=33, index_bytes=100,
                           rss_before=1024, rss_after=2048, correctness=CORRECTNESS)
                stdout = 'catalog_marking_sample ' + json.dumps(row) + '\ntest result: ok. 1 passed; 0 failed;'
                return subprocess.CompletedProcess(command, 0, stdout, '')

            with mock.patch.object(catalog_marking.subprocess, 'run', side_effect=run), \
                    mock.patch.object(catalog_marking.common, 'environment_metadata', return_value={}):
                catalog_marking.main(['--counts', '1024', '--holds', '1', '--repetitions', '2',
                    '--probe-binary', str(after), '--baseline-binary', str(before),
                    '--no-build', '--output', str(output)])
            result = json.loads(output.read_text())
            self.assertEqual(calls, ['before', 'after', 'after', 'before'])
            self.assertTrue(result['complete'])
            self.assertTrue(result['configuration']['paired'])
            self.assertEqual([s['variant'] for s in result['samples']],
                             ['baseline', 'candidate', 'candidate', 'baseline'])
            self.assertNotEqual(result['artifacts'][0]['sha256'], result['artifacts'][1]['sha256'])
