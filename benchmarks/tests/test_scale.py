import json
import math
import pathlib
import tempfile
import unittest
from unittest import mock

from benchmarks import cli
from benchmarks.suites import scale


class ScaleTests(unittest.TestCase):
    def output(self, samples):
        return '\n'.join('scale_sample ' + json.dumps(sample) for sample in samples) + '\ntest result: ok. 1 passed; 0 failed\n'

    def sample(self, operation, **extra):
        return dict(status='ok', operation=operation, wall_seconds=0.1, **extra)

    def test_history_requires_every_checkpoint_and_operation_once(self):
        samples = [self.sample(op, generations=n) for n in [100, 1000, 10000] for op in ['tiny-delta-publication', 'reopen']]
        self.assertEqual(scale.parse_samples(self.output(samples), 'history', [100,1000,10000]), samples)
        for broken in [samples[:-1], samples + samples[:1]]:
            with self.assertRaisesRegex(scale.common.BenchmarkError, 'missing or duplicate'):
                scale.parse_samples(self.output(broken), 'history', [100,1000,10000])

    def test_cache_requires_all_patterns_and_both_passes(self):
        samples = [self.sample(op) for op in sorted(scale.OPERATIONS)]
        self.assertEqual(len(scale.parse_samples(self.output(samples), 'pack-cache')), 6)
        with self.assertRaises(scale.common.BenchmarkError):
            scale.parse_samples(self.output(samples[:-1]), 'pack-cache')

    def test_snapshot_metrics_are_complete_nonnegative_integer_counters(self):
        update = dict.fromkeys(scale.SNAPSHOT_METRICS, 1)
        samples = [self.sample('tiny-delta-publication', generations=100, updates=[update]),
                   self.sample('reopen', generations=100)]
        self.assertEqual(scale.parse_samples(self.output(samples), 'history', [100]), samples)
        for value in [-1, 1.5, True, None]:
            broken = {**update, 'catalog_snapshot_nanos': value}
            samples[0]['updates'] = [broken]
            with self.assertRaisesRegex(scale.common.BenchmarkError, 'snapshot metrics'):
                scale.parse_samples(self.output(samples), 'history', [100])
        samples[0]['updates'] = [update, {}]
        with self.assertRaisesRegex(scale.common.BenchmarkError, 'snapshot metrics'):
            scale.parse_samples(self.output(samples), 'history', [100])

    def test_failure_or_missing_native_test_cannot_be_a_result(self):
        samples = [self.sample(op) for op in sorted(scale.OPERATIONS)]
        for value in [-1, math.inf, math.nan]:
            samples[0]['wall_seconds'] = value
            with self.assertRaises(scale.common.BenchmarkError):
                scale.parse_samples(self.output(samples), 'pack-cache')
        samples[0]['wall_seconds'] = 0.1
        with self.assertRaises(scale.common.BenchmarkError):
            scale.parse_samples(self.output(samples).replace('1 passed', '0 passed'), 'pack-cache')

    def test_publication_phases_preserve_complete_nonoverlapping_measurements(self):
        update = dict(publication_phases={name: dict(calls=1, nanos=2) for name in scale.PUBLICATION_PHASES},
                      publish_nanos=20, catalog_build_calls=1, catalog_build_nanos=1)
        samples = [self.sample('tiny-delta-publication', generations=100, updates=[update], coordinates_payload_catalog=False),
                   self.sample('reopen', generations=100)]
        self.assertEqual(scale.parse_samples(self.output(samples), 'history', [100]), samples)
        for value in [-1, 0.5, True, None]:
            bad = json.loads(json.dumps(samples))
            bad[0]['updates'][0]['publication_phases']['state_commit']['nanos'] = value
            with self.assertRaises(scale.common.BenchmarkError):
                scale.parse_samples(self.output(bad), 'history', [100])
        for change in ['missing_phase', 'overlap', 'unexecuted', 'missing_update', 'missing_mode', 'missing_build']:
            bad = json.loads(json.dumps(samples))
            current = bad[0]['updates'][0]
            if change == 'missing_phase':
                del current['publication_phases']['snapshot']
            elif change == 'overlap':
                current['publish_nanos'] = 1
            elif change == 'unexecuted':
                current['publication_phases']['snapshot']['calls'] = 0
            elif change == 'missing_update':
                bad[0]['updates'].append({})
            elif change == 'missing_mode':
                del bad[0]['coordinates_payload_catalog']
            else:
                del current['catalog_build_nanos']
            with self.assertRaises(scale.common.BenchmarkError):
                scale.parse_samples(self.output(bad), 'history', [100])

    def test_registry_selects_all_three_modes(self):
        entries = {entry['id']:entry for entry in cli.entrypoints()}
        for identifier, mode in [('history-scale','history'),('pack-cache-scale','pack-cache'),('network-scale','network')]:
            args = scale.build_parser().parse_args(entries[identifier]['default_arguments'] + ['--output','result.json'])
            self.assertEqual(args.mode, mode)

    def test_failed_matrix_keeps_partial_results_and_error(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = pathlib.Path(temporary) / 'result.json'
            def failed(args, work, result):
                result['samples'].append(self.sample('reopen', generations=100, repetition=1))
                raise scale.common.BenchmarkError('failed at 1000')
            with mock.patch.object(scale, 'native', side_effect=failed), mock.patch.object(scale.common, 'environment_metadata', return_value={}):
                with self.assertRaisesRegex(scale.common.BenchmarkError, 'failed at 1000'):
                    scale.main(['--output',str(output)])
            result = json.loads(output.read_text())
            self.assertFalse(result['complete'])
            self.assertEqual(len(result['samples']),1)
            self.assertEqual(result['error'],'failed at 1000')

    def test_invalid_network_axes_are_rejected(self):
        for value in ['0,0','-1,20']:
            with self.assertRaises(scale.common.BenchmarkError):
                scale.nonnegative_csv(value)
        self.assertEqual(scale.nonnegative_csv('0,25,100'),[0,25,100])

    def test_dashboard_never_merges_different_network_or_cache_sizes(self):
        from benchmarks import dashboard
        with tempfile.TemporaryDirectory() as temporary:
            path = pathlib.Path(temporary) / 'scale.json'
            samples = [self.sample('atomic-rpc-cold', rtt_ms=25, bandwidth_kib_per_connection=rate, repetition=1) for rate in [0,1024,8192]]
            result = dict(result_schema='casita.scale.v1', suite_id='transfer', complete=True, configuration={}, samples=samples)
            path.write_text(json.dumps(result))
            self.assertEqual(len(dashboard.normalize_result(path)['observations']),3)
            result['complete'] = False
            path.write_text(json.dumps(result))
            with self.assertRaisesRegex(ValueError,'incomplete'):
                dashboard.normalize_result(path)

    def test_dashboard_preserves_history_layout_and_transfer_fixture(self):
        from benchmarks import dashboard
        with tempfile.TemporaryDirectory() as temporary:
            path = pathlib.Path(temporary) / 'scale.json'
            for field, values, operation in [
                ('seed_batch_size', [1,64], 'tiny-delta-publication'),
                ('working_set_bytes', [4194304,33554432], 'random-warm'),
                ('subtree_files', [8,64], 'atomic-rpc-cold'),
            ]:
                samples = [self.sample(operation, **{field: value}) for value in values for _ in range(2)]
                path.write_text(json.dumps(dict(result_schema='casita.scale.v1', suite_id='huge-repositories', complete=True, configuration={}, samples=samples)))
                observations = dashboard.normalize_result(path)['observations']
                self.assertEqual(len(observations),2)
                self.assertTrue(all(item['samples'] == 2 for item in observations))
