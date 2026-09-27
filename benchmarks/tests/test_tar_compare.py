import json
import pathlib
import tempfile
import unittest
from unittest import mock

from benchmarks import tar_compare


class TarComparisonTests(unittest.TestCase):
    def setUp(self):
        # These tests exercise reporting and policy, without sampling the host.
        read_text = pathlib.Path.read_text
        exists = pathlib.Path.exists
        readers = mock.patch.object(
            pathlib.Path, 'read_text',
            lambda path, *args, **kwargs: 'test CPU' if str(path) == '/proc/cpuinfo'
            else read_text(path, *args, **kwargs))
        paths = mock.patch.object(
            pathlib.Path, 'exists',
            lambda path: True if str(path) == '/proc/self/stat' else exists(path))
        readers.start()
        paths.start()
        self.addCleanup(readers.stop)
        self.addCleanup(paths.stop)

    def test_cpu_limit_rejects_invalid_values_before_creating_output(self):
        for value in ('-1', '101', 'nan', 'inf'):
            with self.subTest(value=value), mock.patch.object(tar_compare.sys, 'stderr'), \
                 self.assertRaises(SystemExit) as error:
                tar_compare.main(['--binary', 'unused', '--output', 'unused',
                                  '--max-external-cpu-percent', value])
            self.assertEqual(error.exception.code, 2)

    def test_thirty_percent_policy_keeps_cpu_boundary_and_build_veto(self):
        monitor = tar_compare.QuietHost(max_cpu_fraction=0.3)
        for cpu, builds, expected in ((0.299, [], True), (0.3, [], True),
                                     (0.301, [], False), (0.1, [{'name': 'rustc'}], False)):
            with self.subTest(cpu=cpu, builds=builds):
                self.assertEqual(monitor.quiet({'external_cpu_fraction': cpu,
                                               'competing_processes': builds}), expected)
        monitor = tar_compare.QuietHost(max_cpu_fraction=0.3, allow_competing_builds=True)
        for cpu, expected in ((0.299, True), (0.3, True), (0.301, False)):
            self.assertEqual(monitor.quiet({'external_cpu_fraction': cpu,
                                           'competing_processes': [{'name': 'rustc'}]}), expected)

    def test_paired_binaries_reverse_order_and_keep_matched_estimates(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            baseline, candidate = directory / 'baseline', directory / 'candidate'
            baseline.write_bytes(b'baseline')
            candidate.write_bytes(b'candidate')
            output = directory / 'report'
            monitor = mock.MagicMock()
            monitor.report.return_value = {'quiet': True}
            invocations = []
            def execute(command, **kwargs):
                invocations.append((command, kwargs['env']))
            def cases(value):
                return [{'id': 'case', 'estimates_ns': {'mean': {'point_estimate': value}}}]
            with mock.patch.object(tar_compare.platform, 'platform', return_value='test'), \
                 mock.patch.object(tar_compare, 'QuietHost', return_value=monitor) as guard, \
                 mock.patch.object(tar_compare.subprocess, 'run', side_effect=execute), \
                 mock.patch.object(tar_compare, 'collect', side_effect=[cases(100), cases(80), cases(90), cases(100)]), \
                 mock.patch.dict(tar_compare.os.environ, {'CASITA_BENCH_PERF_CONTROL': 'stale', 'CASITA_BENCH_PERF_ACK': 'stale'}):
                code = tar_compare.main(['--binary', str(candidate), '--baseline-binary', str(baseline),
                                         '--output', str(output), '--repetitions', '2',
                                         '--max-external-cpu-percent', '30', '--allow-competing-builds'])
            self.assertEqual(code, 0)
            self.assertEqual(guard.call_args_list,
                             [mock.call(timeout=60, max_cpu_fraction=0.3, allow_competing_builds=True)] * 4)
            self.assertEqual([command[0] for command, _ in invocations],
                             [str(path.resolve()) for path in (baseline, candidate, candidate, baseline)])
            self.assertEqual([env.get('CASITA_TAR_REVERSE') for _, env in invocations], [None, None, '1', '1'])
            self.assertEqual(len({env['CRITERION_HOME'] for _, env in invocations}), 4)
            self.assertTrue(all('CASITA_BENCH_PERF_CONTROL' not in env and 'CASITA_BENCH_PERF_ACK' not in env
                                for _, env in invocations))
            report = json.loads((output / 'report.json').read_text())
            self.assertTrue(report['complete'])
            self.assertEqual(report['configuration']['max_external_cpu_percent'], 30)
            self.assertIn('<=30%', report['host_policy'])
            self.assertTrue(report['configuration']['allow_competing_builds'])
            self.assertIn('without a separate veto', report['host_policy'])
            self.assertAlmostEqual(report['comparison'][0]['median_paired_time_ratio'], 0.85)

    def test_comparison_rejects_incomplete_contended_and_mismatched_pairs(self):
        def run(variant, identifier='case', value=100, status='accepted'):
            return {'repetition': 0, 'variant': variant, 'status': status,
                    'cases': [{'id': identifier, 'estimates_ns': {'mean': {'point_estimate': value}}}]}
        for runs in ([run('baseline')],
                     [run('baseline'), run('candidate', status='contended')],
                     [run('baseline'), run('candidate', identifier='other')],
                     [run('baseline'), run('candidate', value=float('nan'))],
                     [run('baseline'), run('candidate', value=0)],
                     [run('baseline'), run('candidate'), run('candidate')]):
            with self.subTest(runs=runs), self.assertRaises(ValueError):
                tar_compare.paired_comparison(runs, 1)

    def test_missing_or_duplicate_cases_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            with self.assertRaises(ValueError):
                tar_compare.collect(directory)
            ids = [f'tar_import_pipeline/{shape}/{limit}'
                   for shape in ('small-0', 'small-1', 'small-15', 'small-16', 'small-17', 'small-256', 'large', 'mixed')
                   for limit in (1, 16)]
            for index, identifier in enumerate(ids):
                result = directory / str(index) / 'new'
                result.mkdir(parents=True)
                (result / 'benchmark.json').write_text(json.dumps({'full_id': identifier}))
                (result / 'estimates.json').write_text('{}')
                (result / 'sample.json').write_text('{}')
            self.assertEqual(len(tar_compare.collect(directory)), 16)
            (directory / '0/new/benchmark.json').write_text(json.dumps({'full_id': ids[1]}))
            with self.assertRaises(ValueError):
                tar_compare.collect(directory)

    def test_alternates_order_and_retains_contended_measurements(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            binary = directory / 'binary'
            binary.write_bytes(b'unchanged')
            output = directory / 'report'
            monitor = mock.MagicMock()
            monitor.report.side_effect = [{'quiet': True}, {'quiet': False}]
            environments = []
            def execute(*args, **kwargs):
                environments.append(kwargs['env'])
            with mock.patch.object(tar_compare.platform, 'platform', return_value='test'), \
                 mock.patch.object(tar_compare, 'QuietHost', return_value=monitor), \
                 mock.patch.object(tar_compare.subprocess, 'run', side_effect=execute), \
                 mock.patch.object(tar_compare, 'collect', return_value=[{'samples': 'retained'}]):
                code = tar_compare.main(['--binary', str(binary), '--output', str(output), '--repetitions', '2'])
            self.assertEqual(code, 1)
            self.assertNotIn('CASITA_TAR_REVERSE', environments[0])
            self.assertEqual(environments[1]['CASITA_TAR_REVERSE'], '1')
            report = json.loads((output / 'report.json').read_text())
            self.assertFalse(report['complete'])
            self.assertEqual([run['status'] for run in report['runs']], ['accepted', 'contended'])
            self.assertEqual(report['runs'][1]['cases'], [{'samples': 'retained'}])

    def test_quiet_timeout_records_failure_without_running_benchmark(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = pathlib.Path(temporary)
            binary = directory / 'binary'
            binary.write_bytes(b'unchanged')
            output = directory / 'report'
            monitor = mock.MagicMock()
            del monitor.wait_seconds
            activity = {'external_cpu_fraction': 0.4, 'competing_processes': [{'name': 'rustc'}]}
            monitor.sample.return_value = activity
            def reject_host():
                monitor.sample()
                raise RuntimeError('host did not become quiet')
            monitor.__enter__.side_effect = reject_host
            with mock.patch.object(tar_compare.platform, 'platform', return_value='test'), \
                 mock.patch.object(tar_compare, 'QuietHost', return_value=monitor), \
                 mock.patch.object(tar_compare.subprocess, 'run') as run:
                self.assertEqual(tar_compare.main(['--binary', str(binary), '--output', str(output)]), 1)
            run.assert_not_called()
            report = json.loads((output / 'report.json').read_text())
            self.assertEqual(report['runs'][0]['error'], 'host did not become quiet')
            self.assertEqual(report['runs'][0]['activity_intervals'], [activity])
            self.assertFalse(report['complete'])
