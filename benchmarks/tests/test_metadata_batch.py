import copy
import itertools
import json
import pathlib
import tempfile
import unittest
from unittest import mock

from benchmarks import all as all_suites, cli, dashboard, revisions
from benchmarks.suites import metadata_batch as batch


class MetadataBatchTests(unittest.TestCase):
    def case(self, count=512, widths=None, requests=1024, iterations=1):
        widths = widths or batch.WIDTHS
        return dict(count=count, widths=widths, requests=requests, iterations=iterations,
                    correctness=batch.CORRECTNESS,
                    samples=[dict(pattern=p, width=w, iteration=i, variant=v, warm=i > 0, nanos=1000 * (i + 1))
                             for p, w, i, v in itertools.product(batch.PATTERNS, widths, range(iterations + 1), batch.VARIANTS)])

    def output(self, case):
        return 'batch_sample ' + json.dumps(case) + '\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n'

    def parse(self, case):
        return batch.parse_sample(self.output(case), 512, batch.WIDTHS, 1024, 1)

    def test_exact_matrix_gate_and_timings(self):
        good = self.case()
        self.assertEqual(self.parse(good), good)
        for field, value in [('correctness', None), ('count', 513), ('requests', 1023),
                             ('widths', [256]), ('samples', good['samples'][:-1]),
                             ('samples', good['samples'] + [good['samples'][0]])]:
            with self.subTest(field=field), self.assertRaises(batch.common.BenchmarkError):
                self.parse({**good, field: value})
        for field, value in [('nanos', -1), ('nanos', True), ('nanos', 0.5), ('warm', True),
                             ('width', True), ('iteration', True), ('variant', 'unknown')]:
            bad = copy.deepcopy(good)
            bad['samples'][0][field] = value
            with self.subTest(field=field, value=value), self.assertRaises(batch.common.BenchmarkError):
                self.parse(bad)
        with self.assertRaises(batch.common.BenchmarkError):
            batch.parse_sample(self.output(good).replace('1 passed', '0 passed'), 512, good['widths'], 1024, 1)

    def run_fixture(self, root, fail=False):
        binary = root / 'probe'
        binary.write_bytes(b'fixture probe')
        output = root / 'result.json'

        def measured(command, stdout, stderr, **kwargs):
            self.assertIn(batch.PROBE, command.steps[0])
            env = command.env
            stdout.write_text(self.output(self.case(int(env['CASITA_BATCH_COUNT']),
                list(map(int, env['CASITA_BATCH_WIDTHS'].split(','))),
                int(env['CASITA_BATCH_REQUESTS']), int(env['CASITA_BATCH_ITERATIONS']))))
            stderr.write_text('fixture failure' if fail else '')
            return dict(exit_code=7 if fail else 0, wall_seconds=99, max_rss_bytes=1234)

        with mock.patch.object(batch.common, 'measured_command', side_effect=measured), \
             mock.patch.object(batch.common, 'environment_metadata', return_value={}):
            args = ['--profile', 'smoke', '--repetitions', '1', '--probe-binary', str(binary), '--no-build', '--output', str(output)]
            if fail:
                with self.assertRaisesRegex(batch.common.BenchmarkError, 'probe failed'):
                    batch.main(args)
            else:
                self.assertEqual(batch.main(args), 0)
        return output

    def test_smoke_normalization_keeps_all_dimensions_and_variants(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = self.run_fixture(pathlib.Path(temporary))
            raw = json.loads(output.read_text())
            self.assertTrue(raw['complete'])
            self.assertEqual(len(raw['samples']), 64)
            self.assertEqual(len(raw['artifacts'][0]['sha256']), 64)
            observations = dashboard.normalize_result(output)['observations']
            self.assertEqual(len(observations), 64)
            self.assertEqual({o['scale']['batch_width'] for o in observations}, set(batch.WIDTHS))
            self.assertEqual({o['implementation'] for o in observations}, {'casita-current', 'casita-point-reference'})
            for o in observations:
                self.assertEqual(o['metrics']['wall_seconds'], 1e-6 if o['cache_policy'] == 'first' else 2e-6)
                self.assertEqual(o['scale']['requests'], 1024)
            raw['complete'] = False
            output.write_text(json.dumps(raw))
            with self.assertRaisesRegex(ValueError, 'incomplete'):
                dashboard.normalize_result(output)

    def test_failure_retains_receipt_and_rejects_comparison(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = self.run_fixture(pathlib.Path(temporary), fail=True)
            raw = json.loads(output.read_text())
            self.assertFalse(raw['complete'])
            self.assertEqual(raw['samples'], [])
            self.assertEqual(raw['processes'][0]['exit_code'], 7)
            with self.assertRaisesRegex(ValueError, 'incomplete'):
                dashboard.normalize_result(output)

    def test_registered_for_all_and_revision_runner(self):
        entry = next(e for e in cli.entrypoints() if e['id'] == 'metadata-batch')
        args = batch.build_parser().parse_args(all_suites.suite_arguments(
            entry['id'], pathlib.Path('/binaries'), 'smoke', 1) + ['--output', '/result.json'])
        self.assertEqual(args.probe_binary, pathlib.Path('/binaries/casita-lib-test'))
        self.assertTrue(args.no_build)
        self.assertEqual(revisions.SUITE_BUILD_SPECS[entry['id']].cargo_arguments, batch.CARGO_ARGUMENTS)
