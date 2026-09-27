import pathlib
import tempfile
import unittest
from unittest import mock

from benchmarks import obrador_profile as profile
from benchmarks.suites.repository import BenchmarkError
from benchmarks.suites.obrador_reads import use_durable_admission


class ObradorProfileTests(unittest.TestCase):
    def test_durable_admission_supports_source_layouts(self):
        marker = 'self.owned_retention_hold_kind(true).await'
        for relative in ('src/repository.rs', 'src/repository/retention.rs', 'crates/casita/src/repository/retention.rs'):
            with self.subTest(layout=relative), tempfile.TemporaryDirectory() as directory:
                source = pathlib.Path(directory)
                repository = source / relative
                repository.parent.mkdir(parents=True)
                repository.write_text(marker)
                if relative.startswith("crates/"):
                    (source / "crates/casita/Cargo.toml").write_text("[package]\nname = \"casita\"\n")
                use_durable_admission(source)
                self.assertEqual(repository.read_text(), marker.replace('true', 'false'))

    def test_durable_admission_rejects_missing_or_ambiguous_control(self):
        marker = 'self.owned_retention_hold_kind(true).await'
        for code in ('unrelated code', marker + '\n' + marker):
            with self.subTest(code=code), tempfile.TemporaryDirectory() as directory:
                source = pathlib.Path(directory)
                repository = source / 'src/repository/retention.rs'
                repository.parent.mkdir(parents=True)
                repository.write_text(code)
                with self.assertRaises(BenchmarkError):
                    use_durable_admission(source)
                self.assertEqual(repository.read_text(), code)

    def test_rebuild_casita_preserves_obrador_lockfile_and_old_binaries(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            work, source = root / 'new', root / 'source'
            work.mkdir()
            (source / 'src').mkdir(parents=True)
            marker = 'self.owned_retention_hold_kind(true).await'
            (source / 'src/repository.rs').write_text(marker)
            previous = {'binaries': {}}
            for variant in ('durable', 'process'):
                obrador = root / variant / 'obrador'
                (obrador / 'obrador-core/examples').mkdir(parents=True)
                (obrador / 'obrador-core/examples/casita-retained-reads.rs').write_text('old probe')
                (obrador / 'Cargo.lock').write_text('fixed lock')
                (obrador / 'Cargo.toml').write_text('casita = { path = "../casita" }\n')
                (obrador / 'implementation.rs').write_text('fixed API')
                casita = obrador.parent / 'casita'
                casita.mkdir()
                (casita / 'old.rs').write_text('old core')
                binary = root / f'obrador-reads-{variant}'
                binary.write_bytes(b'old binary')
                previous['binaries'][variant] = {'path': str(binary)}
            def build(command, **kwargs):
                self.assertIn('--locked', command)
                target = pathlib.Path(command[command.index('--target-dir') + 1])
                artifact = target / 'release/examples/casita-retained-reads'
                artifact.parent.mkdir(parents=True, exist_ok=True)
                artifact.write_bytes(b'new binary')
            with mock.patch.object(profile.subprocess, 'run', side_effect=build):
                binaries, _ = profile.rebuild(previous, work, source)
            for variant in ('durable', 'process'):
                self.assertEqual((root / variant / 'obrador/Cargo.lock').read_text(), 'fixed lock')
                self.assertEqual((root / variant / 'obrador/implementation.rs').read_text(), 'fixed API')
                expected = marker if variant == 'process' else marker.replace('true', 'false')
                self.assertEqual((root / variant / 'casita/src/repository.rs').read_text(), expected)
                self.assertEqual((work / f'previous-casita-{variant}/old.rs').read_text(), 'old core')
                self.assertEqual((root / f'obrador-reads-{variant}').read_bytes(), b'old binary')
                self.assertEqual(pathlib.Path(binaries[variant]['path']).read_bytes(), b'new binary')
            self.assertEqual((source / 'src/repository.rs').read_text(), marker)

    def test_rejects_unscoped_and_empty_profiles(self):
        with tempfile.TemporaryDirectory() as directory:
            log = pathlib.Path(directory) / 'case.log'
            with self.assertRaises(BenchmarkError):
                profile.report('perf', log, {})
            def empty(command, **kwargs):
                kwargs['stdout'].write('# Samples: 0 of event cpu-clock:u\n')
            with mock.patch.object(profile.subprocess, 'run', side_effect=empty), \
                 self.assertRaises(BenchmarkError):
                profile.report('perf', log, {'profiled_read_phase': True})

    def test_requires_acknowledged_enable_and_keeps_data_hash(self):
        with tempfile.TemporaryDirectory() as directory:
            log = pathlib.Path(directory) / 'case.log'
            command, environment = profile.command('perf', ['/probe', '100', '8', '200', 'false'], log)
            self.assertIn('--delay=-1', command)
            self.assertTrue(pathlib.Path(environment['CASITA_BENCH_PERF_CONTROL']).is_fifo())
            self.assertTrue(pathlib.Path(environment['CASITA_BENCH_PERF_ACK']).is_fifo())
            log.with_suffix('.perf.data').write_bytes(b'profile')
            def recorded(command, **kwargs):
                kwargs['stdout'].write('# Samples: 1K of event cpu-clock:u\n# Event count (approx.): 1234\n'
                    ' 10.00% worker binary [.] _blake3_hash_many\n'
                    ' 20.00% worker binary [.] casita::metadata::pins::ReaderState::encode\n')
            with mock.patch.object(profile.subprocess, 'run', side_effect=recorded):
                result = profile.report('perf', log, {'profiled_read_phase': True})
            self.assertEqual(len(result['sha256']), 64)
            self.assertEqual(result['scope'], 'timed read/GC phase only')
            self.assertEqual(result['approx_user_cpu_nanos'], 1234)
            self.assertEqual(result['self_sample_percent']['blake3'], 10)
            self.assertEqual(result['self_sample_percent']['casita_pins'], 20)
