import json
import pathlib
import unittest

from benchmarks import all as runner
from benchmarks import cli
from benchmarks.suites import git_ingest_scheduling as suite
from benchmarks.suites import repository as common


class GitIngestTests(unittest.TestCase):
    def test_probe_requires_both_matching_imports_and_a_passing_test(self):
        base = dict(files=17, concurrency=16, max_buffered_bytes=65536, delay_ms=5, packed=True,
                    root='git.view.v1:example', wall_nanos=1, peak_active=16, peak_bytes=65536,
                    correctness=suite.CORRECTNESS)
        rows = [{**base, 'operation': operation} for operation in ('initial-import', 'incremental-import')]

        def parse(values, footer='test result: ok. 1 passed; 0 failed;'):
            return suite.parse_samples('\n'.join('git_ingest_sample ' + json.dumps(row) for row in values) + '\n' + footer,
                                       17, 16, 65536, 5, True)

        self.assertEqual(parse(rows), rows)
        for field, value in (('files', 16), ('concurrency', 1), ('max_buffered_bytes', 1),
                             ('delay_ms', 0), ('packed', 1), ('root', ''), ('operation', 'initial-import'),
                             ('wall_nanos', True), ('peak_active', 17), ('peak_bytes', 0), ('correctness', '')):
            with self.subTest(field=field), self.assertRaises(common.BenchmarkError):
                parse([rows[0], {**rows[1], field: value}])
        with self.assertRaises(common.BenchmarkError):
            parse(rows[:1])
        with self.assertRaises(common.BenchmarkError):
            parse(rows, 'test result: ok. 0 passed; 0 failed;')

    def test_all_supplies_binaries_for_both_registered_suites(self):
        for name, binary in (('git-ingest-concurrency', 'casita'), ('git-ingest-scheduling', 'casita-lib-test')):
            entry = next(entry for entry in cli.entrypoints() if entry['id'] == name)
            self.assertEqual(entry['suite_id'], 'native-git')
            args = runner.suite_arguments(name, pathlib.Path('/binaries'), 'smoke', 1)
            self.assertIn('/binaries/' + binary, args)
            self.assertIn('--no-build', args)
