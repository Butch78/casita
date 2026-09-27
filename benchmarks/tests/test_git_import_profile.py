import json
import pathlib
import tempfile
import unittest

from benchmarks import all as runner, cli, dashboard
from benchmarks.suites import git_import_profile as suite
from benchmarks.suites import repository as common


class ProfileTests(unittest.TestCase):
    def test_dashboard_keeps_shapes_and_import_rss(self):
        result = dict(result_schema='casita.git-import-profile.v1', suite_id='native-git', complete=True,
                      configuration=dict(profile='smoke'), samples=[
                          dict(shape=shape, operation='initial-import', status='ok', entries=10,
                               wall_seconds=1, peak_rss_at_import_end_bytes=4096)
                          for shape in ('many-objects', 'delta-heavy')])
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory)/'result.json'
            path.write_text(json.dumps(result))
            observations = dashboard.normalize_result(path)['observations']
            self.assertEqual(len(observations), 2)
            self.assertEqual({row['scale']['shape'] for row in observations}, {'many-objects', 'delta-heavy'})
            self.assertTrue(all(row['metrics']['max_rss_bytes'] == 4096 for row in observations))
            result['complete'] = False
            path.write_text(json.dumps(result))
            with self.assertRaises(ValueError):
                dashboard.normalize_result(path)

    def test_parser_requires_phase_counters_and_correctness(self):
        row = dict(operation='initial-import', objects=67, concurrency=16, max_buffered_bytes=67108864,
                   phase_names=suite.PHASES, wall_nanos=1, correctness=suite.CORRECTNESS,
                   root='view-key', profile=dict(calls=[0]*12, nanos=[0]*12, peak_active=16,
                                                decoded_objects=67, decoded_bytes=1000, peak_buffered_bytes=1000))
        def parse(value, footer='test result: ok. 1 passed; 0 failed;'):
            return suite.parse_sample('git_import_profile '+json.dumps(value)+'\n'+footer, 'initial-import', {'objects':67})
        self.assertEqual(parse(row), row)
        for field, value in [('objects', 66), ('concurrency', 1), ('max_buffered_bytes', 1), ('phase_names', []), ('wall_nanos', True), ('correctness', '')]:
            with self.subTest(field=field), self.assertRaises(common.BenchmarkError):
                parse({**row, field:value})
        for field, value in [('calls',[0]), ('nanos',[-1]*12), ('peak_active',17), ('decoded_objects',66), ('decoded_bytes',-1)]:
            with self.subTest(field=field), self.assertRaises(common.BenchmarkError):
                parse({**row, 'profile':{**row['profile'],field:value}})
        with self.assertRaises(common.BenchmarkError):
            parse(row, 'test result: ok. 0 passed; 0 failed;')

    def test_all_supplies_a_git_enabled_probe(self):
        from benchmarks.revisions import SUITE_BUILD_SPECS
        entry=next(e for e in cli.entrypoints() if e['id']=='git-import-profile')
        self.assertEqual(entry['suite_id'], 'native-git')
        self.assertIn('cli,git', SUITE_BUILD_SPECS[entry['id']].cargo_arguments)
        args=runner.suite_arguments(entry['id'],pathlib.Path('/binaries'),'smoke',1)
        self.assertIn('/binaries/casita-lib-test',args)
        self.assertIn('--no-build',args)
