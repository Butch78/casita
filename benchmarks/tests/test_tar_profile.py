import unittest

from benchmarks.tar_profile import summarize


class TarProfileTests(unittest.TestCase):
    def test_rejects_empty_events_and_missing_symbols(self):
        for report in ('', '# Event count (approx.): 0\n', '# Event count (approx.): 100\n',
                       '# Samples: 9 of event cpu-clock:u\n# Event count (approx.): 100\n'):
            with self.assertRaises(ValueError):
                summarize(report)

    def test_categorizes_self_cost_without_double_counting(self):
        result = summarize('# Samples: 1K of event cpu-clock:u\n# Event count (approx.): 1234000\n'
            ' 35.00% worker binary [.] _blake3_hash_many_avx512\n'
            ' 10.00% worker binary [.] ZSTD_compressBlock_doubleFast\n'
            '  2.00% worker binary [.] FSE_compress\n'
            '  5.00% worker binary [.] _int_malloc\n'
            '  3.00% worker binary [.] casita::metadata::snapshot\n'
            '  4.00% worker binary [.] tokio::runtime::task::poll\n'
            '  6.00% worker binary [.] fastcdc::v2020::cut_gear\n'
            ' 35.00% worker binary [.] __memmove_avx512_unaligned_erms\n')
        self.assertEqual(result['approx_user_cpu_ns'], 1234000)
        self.assertEqual(result['self_percent_by_symbol_category'],
            dict(blake3=35, zstd=12, allocation=5, metadata=3, runtime=4, fastcdc=6, other=35))
        self.assertEqual(len(result['top_symbols']), 8)
        self.assertEqual(result['approx_samples'], 1000)
        self.assertEqual(result['blake3_self_percent_by_thread_name'], {'worker': 35})
