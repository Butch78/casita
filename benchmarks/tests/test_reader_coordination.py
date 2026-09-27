import json
import pathlib
import tempfile
import unittest

from benchmarks import all as all_suites, dashboard
from benchmarks.suites import reader_coordination as suite
from benchmarks.suites import repository as common


class ReaderCoordinationTests(unittest.TestCase):
    def test_requires_both_reservation_sides_and_exact_durability_gates(self):
        phases = ['cold-register', 'warm-register', 'warm-protect', 'warm-release',
                  'last-reserved-register', 'reservation-rollover-protect', 'renewed-release']
        case = dict(active_readers=64, iterations=1, reservation=65536, correctness=suite.CORRECTNESS,
                    samples=[dict(phase=phase, nanos=100, durable_changed=suite.PHASES[phase]) for phase in phases])
        def parse(value, trailer='test result: ok. 1 passed; 0 failed;'):
            return suite.parse_sample('reader_coordination_sample ' + json.dumps(value) + '\n' + trailer, 64, 1)
        self.assertEqual(parse(case), case)
        for field, value in [('reservation', 65535), ('active_readers', 1), ('iterations', 2),
                             ('correctness', ''), ('samples', case['samples'][:-1])]:
            with self.subTest(field=field), self.assertRaises(common.BenchmarkError):
                parse({**case, field: value})
        for index in range(len(phases)):
            samples = [dict(sample) for sample in case['samples']]
            samples[index]['durable_changed'] = 1 - samples[index]['durable_changed']
            with self.subTest(phase=phases[index]), self.assertRaises(common.BenchmarkError):
                parse({**case, 'samples': samples})
        with self.assertRaises(common.BenchmarkError):
            parse(case, 'test result: ok. 0 passed; 0 failed;')

    def test_dashboard_and_all_registration(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = pathlib.Path(temporary) / 'result.json'
            path.write_text(json.dumps(dict(result_schema='casita.reader-coordination.v1', complete=True,
                suite_id='state-and-publication', environment={}, configuration={},
                samples=[dict(status='ok', operation=phase, readers=readers, wall_seconds=0.1)
                         for phase in suite.PHASES for readers in (1, 64)])))
            self.assertEqual(len(dashboard.normalize_result(path)['observations']), 14)
        args = all_suites.suite_arguments('reader-coordination', pathlib.Path('/binaries'), 'smoke', 1)
        self.assertIn('/binaries/casita-lib-test', args)
        self.assertIn('--no-build', args)
