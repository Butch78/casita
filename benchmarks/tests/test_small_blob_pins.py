import json
import pathlib
import tempfile
import types
import unittest
from unittest import mock

from benchmarks.suites import small_blob_pins as suite


class LayoutEvidence(unittest.TestCase):
    def test_default_matrix_covers_both_layouts_and_cutoff(self):
        self.run_fixture(wrong_layout=False)

    def test_old_probe_cannot_mislabel_packed_measurements_as_loose(self):
        self.run_fixture(wrong_layout=True)

    def run_fixture(self, wrong_layout):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            binary = root / "probe"
            binary.write_bytes(b"fixture")
            seen = []

            def run(command, *, env, **kwargs):
                layout = env["CASITA_BENCH_PIN_LAYOUT"]
                count = int(env["CASITA_BENCH_BLOBS"])
                size = int(env["CASITA_BENCH_BLOB_BYTES"])
                seen.append((layout, size))
                row = dict(layout="packed" if wrong_layout else layout,
                           count=count, bytes=size, nanos=1, ledger_edits=count,
                           correctness=suite.CORRECTNESS)
                return types.SimpleNamespace(returncode=0, stderr="", stdout=
                    "small_blob_pins_sample " + json.dumps(row) +
                    "\ntest result: ok. 1 passed; 0 failed;\n")

            with (mock.patch.object(suite.subprocess, "run", side_effect=run),
                  mock.patch.object(suite.common, "environment_metadata", return_value={})):
                arguments = ["--profile", "smoke", "--repetitions", "1", "--no-build",
                             "--probe-binary", str(binary), "--output", str(root / "out.json")]
                if wrong_layout:
                    with self.assertRaises(suite.common.BenchmarkError):
                        suite.main(arguments)
                else:
                    self.assertEqual(suite.main(arguments), 0)
                    self.assertEqual(set(seen), {(layout, size) for layout in ("packed", "loose")
                                                for size in (64, 511, 512, 513, 2047, 2048, 2049)})
            result = json.loads((root / "out.json").read_text())
            self.assertEqual(result["complete"], not wrong_layout)
