from pathlib import Path
import tempfile
import unittest

from benchmarks.suites import native_fskit_first_launch as suite
from benchmarks.suites.repository import BenchmarkError


class FirstLaunchTests(unittest.TestCase):
    def test_read_preparation_checks_bytes_and_none_does_not_touch_path(self):
        self.assertEqual(suite.prepare(Path('/absent'), 'run', 'none', b'ok'), {'total_ns': 0})
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'run').write_bytes(b'ok')
            self.assertEqual(suite.prepare(root, 'run', 'read', b'ok')['bytes'], 2)
            with self.assertRaisesRegex(BenchmarkError, 'bytes differ'):
                suite.prepare(root, 'run', 'read', b'wrong')

    def test_matrix_requires_unique_paths_all_backends_and_teardown(self):
        rows = [dict(repetition=0, preparation=mode, target=target, implementation=backend,
                     tree=f'/{mode}/{target}/{backend}', correctness='passed', teardown='passed')
                for mode in suite.PREPARATIONS for target, _ in suite.TARGETS for backend in ('native', 'host')]
        suite.validate(rows, 1, ('native', 'host'))
        for invalid in (rows[:-1], rows + [rows[0]],
                        [{**rows[0], 'tree':rows[1]['tree']}, *rows[1:]],
                        [{**rows[0], 'teardown':'pending'}, *rows[1:]]):
            with self.assertRaises(BenchmarkError):
                suite.validate(invalid, 1, ('native', 'host'))


if __name__ == '__main__':
    unittest.main()
