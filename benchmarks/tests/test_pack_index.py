import unittest

from benchmarks.suites.pack import index as pack_index

class PackIndexTests(unittest.TestCase):
    def test_validate_accepts_one_rebuild_and_checkpoint_hits(self):
        cold = {
            "metrics": {
                "pack_index_fallbacks": 1,
                "pack_index_put_requests": 1,
                "pack_index_pointer_requests": 1,
                "pack_footer_range_requests": 6,
                "pack_list_requests": 4,
            }
        }
        warm = [
            {
                "metrics": {
                    "pack_index_hits": 1,
                    "pack_index_fallbacks": 0,
                    "pack_footer_range_requests": 0,
                    "pack_index_pointer_requests": 1,
                    "pack_index_requests": 0,
                    "pack_list_requests": 0,
                }
            }
        ]
        pack_index.validate(cold, warm, 3)

    def test_validate_rejects_warm_footer_reads(self):
        cold = {
            "metrics": {
                "pack_index_fallbacks": 1,
                "pack_index_put_requests": 1,
                "pack_index_pointer_requests": 1,
                "pack_footer_range_requests": 2,
                "pack_list_requests": 4,
            }
        }
        warm = [
            {
                "metrics": {
                    "pack_index_hits": 1,
                    "pack_index_fallbacks": 0,
                    "pack_footer_range_requests": 1,
                    "pack_index_pointer_requests": 1,
                    "pack_index_requests": 0,
                    "pack_list_requests": 0,
                }
            }
        ]
        with self.assertRaises(Exception):
            pack_index.validate(cold, warm, 1)


if __name__ == "__main__":
    unittest.main()
