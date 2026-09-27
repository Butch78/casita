import json
import pathlib
import tempfile
import unittest
from benchmarks import all as all_suites, dashboard
from benchmarks.suites import object_reads as suite
from benchmarks.suites import repository as common


class ObjectReadTests(unittest.TestCase):
    def test_snapshot_admission_and_release_use_process_protection(self):
        for admission in ('cold', 'warm'):
            case = dict(size=64, garbage=4, mode='snapshot', reader_admission=admission,
                        open_durable_revision_changed=int(admission == 'cold'),
                        release_durable_revision_changed=0, correctness=suite.CORRECTNESS,
                        **{f'{phase}_nanos': 100 for phase in suite.PHASES},
                        open_ledger_updates=1, release_ledger_updates=1, gc_removed=0,
                        pack_bytes_before=1000, pack_bytes_after=1000)
            def parse(value):
                return suite.parse_sample('object_read_sample ' + json.dumps(value)
                    + '\ntest result: ok. 1 passed; 0 failed;', 64, 4, 'snapshot', admission)
            self.assertEqual(parse(case), case)
            for field in ('open_durable_revision_changed', 'release_durable_revision_changed'):
                with self.subTest(admission=admission, field=field), self.assertRaises(common.BenchmarkError):
                    parse({**case, field: 1 - case[field]})

    def test_timings_require_gc_and_correctness_gates(self):
        case = dict(size=64, garbage=4, mode='object', reader_admission='cold', open_durable_revision_changed=1, release_durable_revision_changed=0, correctness=suite.CORRECTNESS,
                    **{f'{phase}_nanos': 100 for phase in suite.PHASES},
                    open_ledger_updates=3, release_ledger_updates=1, gc_removed=4,
                    pack_bytes_before=1000, pack_bytes_after=100)
        def parse(value, trailer='test result: ok. 1 passed; 0 failed;'):
            return suite.parse_sample('object_read_sample ' + json.dumps(value) + '\n' + trailer, 64, 4, 'object')
        self.assertEqual(parse(case), case)
        for field, value in [('mode', 'snapshot'), ('garbage', 5), ('correctness', ''),
                             ('gc_removed', 0), ('pack_bytes_after', 1000), ('read_nanos', -1), ('admission_nanos', None), ('reader_admission', 'warm'), ('release_durable_revision_changed', 1), ('open_durable_revision_changed', 0)]:
            with self.subTest(field=field), self.assertRaises(common.BenchmarkError):
                parse({**case, field: value})
        with self.assertRaises(common.BenchmarkError):
            parse(case, 'test result: ok. 0 passed; 0 failed;')

    def test_dimensions_and_all_registration(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = pathlib.Path(temporary) / 'result.json'
            result = dict(result_schema='casita.object-reads.v1', suite_id='repository-e2e', complete=True,
                          environment={}, configuration={}, samples=[dict(status='ok', operation='admission',
                          entries=count, file_bytes=size, variant=mode, reader_admission=admission, wall_seconds=0.1)
                          for count in (1, 4) for size in (64, 1048593) for mode in ('object', 'durable-object', 'snapshot') for admission in ('cold', 'warm')])
            path.write_text(json.dumps(result))
            self.assertEqual(len(dashboard.normalize_result(path)['observations']), 24)
            result['complete'] = False
            path.write_text(json.dumps(result))
            with self.assertRaises(ValueError):
                dashboard.normalize_result(path)
        args = all_suites.suite_arguments('object-reads', pathlib.Path('/binaries'), 'smoke', 1)
        self.assertIn('/binaries/casita-lib-test', args)
        self.assertIn('--no-build', args)
