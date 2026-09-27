import pathlib
import subprocess
import tempfile
import unittest

from benchmarks.suites.git_pack_cached import MODES, mode_order, validate_pack
from benchmarks.suites.git_fetch_s3 import fixture
from benchmarks.suites.git import git_env


class DirectPackTests(unittest.TestCase):
    def test_rotating_order_balances_positions_and_reverses(self):
        orders = [mode_order(i) for i in range(10)]
        for position in range(5):
            self.assertEqual(sorted(row[position] for row in orders), sorted(MODES * 2))
        self.assertEqual(orders[5], list(reversed(orders[0])))

    def test_validation_rejects_truncated_pack(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            source, identity = fixture(root)
            # A real locally generated pack exercises the validator independently
            # of Casita, then a truncated trailer must fail the same gate.
            complete = subprocess.run(["git", "-C", str(source), "pack-objects", "--stdout", "--all"],
                                      check=True, capture_output=True, env=git_env()).stdout
            pack = root / "result.pack"
            pack.write_bytes(complete)
            validate_pack(pack, identity, root)
            pack.write_bytes(complete[:-1])
            with self.assertRaises(RuntimeError):
                validate_pack(pack, identity, root)
