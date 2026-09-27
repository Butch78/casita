import json
import pathlib
import tempfile
import unittest

from benchmarks import all as runner, dashboard
from benchmarks.suites import pin_growth as suite
from benchmarks.suites import repository as common


class PinGrowthTests(unittest.TestCase):
    def test_dashboard_recognizes_growth_results(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory)/'result.json'
            path.write_text(json.dumps(dict(result_schema='casita.pin-growth.v1', suite_id='state-and-publication',
                complete=True, samples=[dict(operation='protect', entries=8192, status='ok', wall_seconds=0.1)])))
            rows = dashboard.normalize_result(path)['observations']
            self.assertEqual(rows[0]['scale']['entries'], 8192)
            self.assertEqual(rows[0]['metrics']['wall_seconds'], 0.1)

    def test_requires_durable_additions_and_replay_gate(self):
        row = dict(resources=8192, iterations=2, nanos=[100, 200], correctness=suite.CORRECTNESS,
                   metrics=dict(operations=2, journal_syncs=2, replacement_updates=0))
        def parse(value):
            return suite.parse_sample('pin_growth_sample '+json.dumps(value)+'\ntest result: ok. 1 passed; 0 failed;', 8192, 2)
        self.assertEqual(parse(row), row)
        for field, value in [('resources', 1), ('correctness', ''), ('nanos', [100]), ('nanos', [0, 100])]:
            with self.subTest(field=field), self.assertRaises(common.BenchmarkError):
                parse({**row, field:value})
        with self.assertRaises(common.BenchmarkError):
            parse({**row, 'metrics': {**row['metrics'], 'journal_syncs':1}})

    def test_all_supplies_probe(self):
        args = runner.suite_arguments('pin-growth', pathlib.Path('/binaries'), 'smoke', 1)
        self.assertIn('/binaries/casita-lib-test', args)
        self.assertIn('--no-build', args)
