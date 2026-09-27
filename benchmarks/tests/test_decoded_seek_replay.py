import json
import unittest
from benchmarks.suites import decoded_seek_replay as suite
from benchmarks.suites.repository import BenchmarkError
from benchmarks import all as all_suites

class ReplayTests(unittest.TestCase):
    def test_result_requires_exact_case_bounds_and_gates(self):
        row=dict(case='launch',capacity=2097152,cycles=17,correctness='passed',release='passed',
                 reader_count=1,shared_cache_bytes=1024,elapsed_ns=1,decode_calls=2,decoded_bytes=1024,fetch_ns=1,admission_ns=1,decode_ns=1,
                 chunk_range_requests=0,cache_bytes=1024,cache_entries=1,returned_bytes=1)
        def output(value): return 'seek_replay_sample '+json.dumps(value)+'\n1 passed;'
        self.assertEqual(suite.parse_sample(output(row),'launch',2097152,17),row)
        for change in ({'case':'sequential'},{'release':'failed'},{'cache_bytes':2097153},
                       {'cache_entries':65},{'shared_cache_bytes':33554433},{'reader_count':2},{'decode_ns':-1},{'elapsed_ns':0}):
            with self.assertRaises(BenchmarkError): suite.parse_sample(output({**row,**change}),'launch',2097152,17)
        with self.assertRaises(BenchmarkError): suite.parse_sample('0 passed;','launch',0,1)

    def test_phase_gate_rejects_a_working_set_that_was_not_prefilled(self):
        row=dict(case='phase-at',capacity=2097152,cycles=17,correctness='passed',release='passed',
                 reader_count=1,shared_cache_bytes=2097152,elapsed_ns=1,decode_calls=136,
                 decoded_bytes=1024,fetch_ns=1,admission_ns=1,decode_ns=1,chunk_range_requests=0,
                 cache_bytes=2097152,cache_entries=1,returned_bytes=1,phase_warm_bytes=2097152,
                 warm_cache_bytes=2097152,warm_elapsed_ns=1,measured_decode_calls=136)
        def parse(value):
            return suite.parse_sample('seek_replay_sample '+json.dumps(value)+'\n1 passed;', 'phase-at',2097152,17)
        self.assertEqual(parse(row),row)
        for changes in ({'warm_cache_bytes':0},{'phase_warm_bytes':2097153},{'warm_elapsed_ns':0}):
            with self.assertRaises(BenchmarkError): parse({**row,**changes})

    def test_permanent_matrix_covers_byte_and_entry_limits(self):
        for limit in ('chunk','working','entries','shared','phase','phase-history','phase-demand','parked'):
            self.assertTrue({limit+'-'+side for side in ('below','at','above')} <= set(suite.CASES))
        args=all_suites.suite_arguments('decoded-seek-replay',suite.Path('/bin'),'smoke',1)
        self.assertIn('--probe-binary',args)
        self.assertIn('--profile',args)

if __name__=='__main__': unittest.main()
