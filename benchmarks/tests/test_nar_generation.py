import json
import pathlib
import tempfile
import unittest
from unittest import mock

from benchmarks import all as runner
from benchmarks.tools.compare_nar_generation import EXPECTED, PARTIAL, collect, main


class NarGenerationTests(unittest.TestCase):
    def test_identical_binaries_are_rejected_before_running(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            baseline, candidate = root / "baseline", root / "candidate"
            baseline.write_bytes(b"same cached executable")
            candidate.write_bytes(b"same cached executable")
            with mock.patch("sys.argv", ["compare", "--baseline-binary", str(baseline),
                            "--candidate-binary", str(candidate), "--output", str(root / "out")]), \
                 mock.patch("subprocess.run") as run:
                with self.assertRaisesRegex(ValueError, "binaries are identical"):
                    main()
                run.assert_not_called()

    def test_missing_measurements_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(RuntimeError, "missing="):
                collect(pathlib.Path(directory))

    def test_samples_and_exact_case_matrix_are_retained(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for identifier in EXPECTED | PARTIAL:
                case = root / identifier / "new"
                case.mkdir(parents=True)
                for name, data in {
                    "benchmark": {"full_id": identifier},
                    "estimates": {"median": {"point_estimate": 42}},
                    "sample": {"iters": [1, 2], "times": [40, 84]},
                }.items():
                    (case / f"{name}.json").write_text(json.dumps(data))
            results = collect(root, EXPECTED | PARTIAL)
            self.assertEqual(set(results), EXPECTED | PARTIAL)
            for case in results.values():
                self.assertEqual(case["samples"]["iters"], [1, 2])
                self.assertEqual(case["estimates_ns"]["median"]["point_estimate"], 42)

    def test_permanent_corpus_includes_generation_cases(self):
        manifest = json.loads((pathlib.Path(__file__).parents[1] / "manifest.json").read_text())
        suite = next(s for s in manifest["suites"] if s["id"] == "core-primitives")
        self.assertIn("nar-invalidation-generation", suite["cases"])
        self.assertIn("nar-partial-hit", suite["cases"])
        self.assertIn("nar_associations", runner.CORE_BENCHES)
        entry = next(e for e in manifest["entrypoints"] if e["id"] == "core-primitives")
        self.assertIn("nar_associations", entry["target"])
