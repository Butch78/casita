import json
import pathlib
import tempfile
import unittest
from unittest import mock

from benchmarks.suites.pack import limits as pack_limits

class PackLimitTests(unittest.TestCase):
    def test_observations_group_repetitions_and_keep_pack_metrics(self):
        sample = {
            "status": "ok",
            "implementation": "casita",
            "corpus": "small-files",
            "cache_policy": "warm",
            "operation": "checkout",
            "wall_seconds": 2.0,
            "max_rss_bytes": 100,
            "repository_usage": {"allocated_bytes": 200},
            "storage_metrics": {
                "pack_count": 3,
                "pack_bytes": 150,
                "largest_pack_bytes": 70,
                "pack_entries": 12,
                "pack_footer_bytes": 24,
                "footer_tail_miss_packs": 1,
                "rebuild_gets_tail_1024k": 4,
                "rebuild_bytes_tail_1024k": 174,
                "rebuild_gets_exact": 6,
                "rebuild_bytes_exact": 72,
                "loose_chunk_count": 0,
            },
        }
        rows = pack_limits.observations(64, {"samples": [sample, {**sample, "wall_seconds": 4.0}]})
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["median_wall_seconds"], 3.0)
        self.assertEqual(rows[0]["median_pack_count"], 3)
        self.assertEqual(rows[0]["median_rebuild_gets_1m"], 4)
        self.assertEqual(rows[0]["median_rebuild_gets_exact"], 6)
        self.assertEqual(rows[0]["median_rebuild_bytes_exact"], 72)

    def test_target_parser_rejects_zero_and_duplicates(self):
        self.assertEqual(pack_limits.positive_targets("1,4,16"), [1, 4, 16])
        with self.assertRaises(Exception):
            pack_limits.positive_targets("0")
        with self.assertRaises(Exception):
            pack_limits.positive_targets("4,4")

    def test_validation_options_are_forwarded_to_repository_suite(self):
        commands = []

        def run(command, check):
            self.assertTrue(check)
            commands.append(command)
            output = pathlib.Path(command[command.index("--output") + 1])
            output.write_text(json.dumps({"samples": []}))

        with tempfile.TemporaryDirectory() as directory:
            output = pathlib.Path(directory) / "limits.json"
            with mock.patch.object(pack_limits.subprocess, "run", side_effect=run):
                self.assertEqual(
                    pack_limits.main(
                        [
                            "--targets-mib", "4",
                            "--casita-fsck-mode", "dry-run",
                            "--skip-post-fsck",
                            "--output", str(output),
                        ]
                    ),
                    0,
                )

        self.assertEqual(
            commands[0][commands[0].index("--casita-fsck-mode") + 1],
            "dry-run",
        )
        self.assertIn("--skip-post-fsck", commands[0])


if __name__ == "__main__":
    unittest.main()
