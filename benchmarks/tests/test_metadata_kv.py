import json
import pathlib
import tempfile
import unittest
from benchmarks import dashboard
from benchmarks.suites import metadata_kv as suite
from benchmarks.suites import repository as common

class MetadataKvTests(unittest.TestCase):
    def test_full_matrix_is_required_before_admitting_timings(self):
        case = dict(count=257, batch=16, iterations=2, value_bytes=256, page_size=16, fanout=257,
                    correctness=suite.CORRECTNESS,
                    samples=[dict(operation=op, iteration=i, nanos=100) for op in suite.OPERATIONS for i in range(2)])
        def parse(value):
            return suite.parse_sample('primitive_sample ' + json.dumps(value) + '\ntest result: ok. 1 passed; 0 failed;\n', 257, 16, 2)
        self.assertEqual(parse(case), case)
        for field, value in [('value_bytes', 4096), ('page_size', 256), ('fanout', 256), ('correctness', ''), ('samples', case['samples'][:-1]), ('samples', case['samples'] + [case['samples'][0]])]:
            with self.subTest(field=field), self.assertRaises(common.BenchmarkError):
                parse({**case, field: value})

    def test_page_and_value_dimensions_never_merge(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = pathlib.Path(temporary) / 'result.json'
            result = dict(result_schema='casita.metadata-kv.v1', suite_id='state-and-publication', complete=True,
                          environment={}, configuration={}, samples=[dict(status='ok', operation='scan-first',
                          entries=257, batch=16, page_size=page, value_bytes=size, wall_seconds=0.1)
                          for page in (16, 256) for size in (256, 4096)])
            path.write_text(json.dumps(result))
            self.assertEqual(len(dashboard.normalize_result(path)['observations']), 4)
            result['complete'] = False
            path.write_text(json.dumps(result))
            with self.assertRaises(ValueError):
                dashboard.normalize_result(path)
