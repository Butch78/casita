from __future__ import annotations

import unittest

from benchmarks.suites import graph


class GraphTraversalTests(unittest.TestCase):
    def test_revision_compatible_fsck_mode_is_explicit(self) -> None:
        args = graph.parse_args(
            ["--output", "result.json", "--casita-fsck-mode", "dry-run"]
        )

        self.assertEqual(args.casita_fsck_mode, "dry-run")


if __name__ == "__main__":
    unittest.main()
