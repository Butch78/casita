import unittest
from unittest import mock

from benchmarks.host_activity import activity


class HostActivityTests(unittest.TestCase):
    def test_stopped_builds_require_stable_identity_state_and_cpu(self):
        before = {10: dict(name='cargo', state='T', parent=0, ticks=10, started=1),
                  11: dict(name='rustc', state='R', parent=10, ticks=10, started=1)}
        after = {pid: dict(p) for pid, p in before.items()}
        row = activity(before, after, 1, 1)
        self.assertEqual(row['paused_build_processes'], [{'pid': 10, 'name': 'cargo'}])
        self.assertEqual(row['competing_processes'], [{'pid': 11, 'name': 'rustc'}])
        for change in ({'state': 'S'}, {'started': 2}, {'ticks': 11}):
            with self.subTest(change=change):
                changed = {10: {**after[10], **change}}
                row = activity(before, changed, 1, 1)
                self.assertEqual(row['competing_processes'], [{'pid': 10, 'name': 'cargo'}])
                self.assertEqual(row['paused_build_processes'], [])
        row = activity(before, {10: {**after[10], 'ticks': 11}}, 1, 1)
        self.assertGreater(row['external_cpu_fraction'], 0)

    def test_excludes_benchmark_descendants_but_detects_other_builds(self):
        def process(name, parent, ticks=100, started=1):
            return dict(name=name, parent=parent, ticks=ticks, started=started)
        before = {1: process('python3', 0), 2: process('obrador-reads-p', 1),
                  3: process('cargo', 0), 4: process('rustc', 3),
                  5: process('chrome', 0)}
        after = {pid: {**p, 'ticks': p['ticks'] + 100} for pid, p in before.items()}
        with mock.patch('os.sysconf', return_value=100), mock.patch('os.cpu_count', return_value=10):
            row = activity(before, after, 1, 1)
        self.assertAlmostEqual(row['external_cpu_fraction'], 0.3)
        self.assertEqual(row['competing_processes'], [{'pid': 3, 'name': 'cargo'}, {'pid': 4, 'name': 'rustc'}])

    def test_pid_reuse_does_not_inflate_cpu_accounting(self):
        before = {5: dict(name='chrome', parent=0, ticks=10, started=1)}
        after = {5: dict(name='chrome', parent=0, ticks=10000, started=2)}
        self.assertEqual(activity(before, after, 1, 1)['external_cpu_fraction'], 0)

    def test_wrapped_cargo_and_hashed_benchmark_names_are_competitors(self):
        after = {pid: dict(name=name, parent=0, ticks=0, started=1)
                 for pid, name in enumerate(['.cargo-wrapped', 'online_holds-48', 'retained_reader',
                                            'clippy-driver', 'rustdoc', 'tar_import-123',
                                            'tar-baseline', 'tar-candidate'], 2)}
        self.assertEqual(len(activity({}, after, 1, 1)['competing_processes']), 8)

    def test_kernel_io_workers_are_recorded_separately(self):
        before = {50: dict(name='kworker', parent=2, ticks=0, started=1),
                  60: dict(name='chrome', parent=0, ticks=0, started=1)}
        after = {pid: {**p, 'ticks': 100} for pid, p in before.items()}
        with mock.patch('os.sysconf', return_value=100), mock.patch('os.cpu_count', return_value=10):
            row = activity(before, after, 1, 1)
        self.assertAlmostEqual(row['external_cpu_fraction'], 0.1)
        self.assertAlmostEqual(row['kernel_worker_cpu_fraction'], 0.1)
        self.assertEqual(row['top_external_processes'], [{'pid': 60, 'name': 'chrome', 'ticks': 100}])
