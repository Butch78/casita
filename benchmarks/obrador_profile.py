"""Scoped CPU profiling of the permanent Obrador read workload."""
import hashlib
import os
import pathlib
import re
import shutil
import subprocess

from benchmarks import cli
from benchmarks.suites.obrador_reads import use_durable_admission
from benchmarks.suites.repository import BenchmarkError


def rebuild(previous, work, casita_source=None):
    """Rebuild the probe and optionally Casita in copied, locked workspaces."""
    workspaces = previous.get('build_workspaces') or {
        variant: {'path': str(pathlib.Path(binary['path']).parent / variant / 'obrador'),
                  'target_dir': str(pathlib.Path(binary['path']).parent / 'target')}
        for variant, binary in previous['binaries'].items()}
    binaries = {}
    for variant, entry in workspaces.items():
        root, target = pathlib.Path(entry['path']), pathlib.Path(entry['target_dir'])
        if casita_source is not None:
            destination = root.parent / 'casita'
            if not destination.exists() or (destination / '.git').exists():
                raise BenchmarkError('Casita replacement requires a copied build workspace')
            shutil.move(str(destination), str(work / f'previous-casita-{variant}'))
            shutil.copytree(casita_source, destination, symlinks=True)
            manifest = root / 'Cargo.toml'
            code = manifest.read_text()
            package = '../casita/crates/casita' if (destination / 'crates/casita/Cargo.toml').exists() else '../casita'
            code, count = re.subn(r'(casita\s*=\s*\{[^\n]*?path\s*=\s*")[^"]*(")',
                          lambda match: match[1] + package + match[2], code)
            if count != 1:
                raise BenchmarkError('cannot locate Obrador Casita dependency path')
            manifest.write_text(code)
            if variant == 'durable':
                use_durable_admission(destination)
        probe = root / 'obrador-core/examples/casita-retained-reads.rs'
        if not probe.exists():
            raise BenchmarkError('rebuilding the probe requires the retained build workspace')
        shutil.copy2(probe, work / f'probe-before-{variant}.rs')
        shutil.copy2(cli.ROOT / 'benchmarks/obrador_reads.rs', probe)
        command = ['cargo', 'build', '--locked', '--release', '--manifest-path', str(root / 'Cargo.toml'),
                   '-p', 'obrador-core', '--example', 'casita-retained-reads', '--target-dir', str(target)]
        print(f'rebuilding {variant} benchmark', flush=True)
        with (work / f'build-{variant}.log').open('w') as log:
            subprocess.run(command, cwd=root, stdout=log, stderr=subprocess.STDOUT, check=True)
        binary = work / f'obrador-reads-{variant}'
        shutil.copy2(target / 'release/examples/casita-retained-reads', binary)
        binaries[variant] = {'path': str(binary), 'sha256': hashlib.sha256(binary.read_bytes()).hexdigest()}
    return binaries, workspaces


def command(perf, command, log):
    control, ack, data = (log.with_suffix(suffix) for suffix in ('.control', '.ack', '.perf.data'))
    os.mkfifo(control)
    os.mkfifo(ack)
    environment = {**os.environ, 'CASITA_BENCH_PERF_CONTROL': str(control),
                   'CASITA_BENCH_PERF_ACK': str(ack)}
    return ([str(perf), 'record', '-e', 'cpu-clock:u', '-F', '499', '--call-graph', 'dwarf,16384',
             '--no-buildid-cache', '--delay=-1', '--control', f'fifo:{control},{ack}',
             '-o', str(data), '--', *command], environment)


def report(perf, log, row):
    if row.get('profiled_read_phase') is not True:
        raise BenchmarkError('scoped profiling requires --rebuild-probe or a fresh probe build')
    data, report_path = log.with_suffix('.perf.data'), log.with_suffix('.perf.txt')
    with report_path.open('w') as stream:
        subprocess.run([str(perf), 'report', '--stdio', '--no-children', '--percent-limit', '0.5',
                        '-i', str(data)], stdout=stream, stderr=subprocess.STDOUT, check=True)
    samples = re.search(r'^# Samples:\s+([0-9][0-9,.]*[KMGT]?)', report_path.read_text(), re.MULTILINE)
    if samples is None or float(samples[1].rstrip('KMGT').replace(',', '')) <= 0:
        raise BenchmarkError('perf did not record any read-phase samples')
    flat_path = log.with_suffix('.perf-flat.txt')
    with flat_path.open('w') as stream:
        subprocess.run([str(perf), 'report', '--stdio', '--no-children', '--call-graph', 'none',
                        '--percent-limit', '0', '-i', str(data)],
                       stdout=stream, stderr=subprocess.STDOUT, check=True)
    flat = flat_path.read_text()
    categories = {'blake3': 0.0, 'casita_pins': 0.0, 'turso': 0.0, 'other': 0.0}
    for percentage, symbol in re.findall(r'^\s+(\d+\.\d+)%\s+\S+\s+\S+\s+\[\.\]\s+(.*)$', flat, re.MULTILINE):
        category = ('blake3' if 'blake3' in symbol else 'casita_pins'
                    if 'casita::metadata::pins' in symbol else 'turso' if 'turso' in symbol else 'other')
        categories[category] += float(percentage)
    event_count = re.search(r'^# Event count \(approx\.\):\s+(\d+)', flat, re.MULTILINE)
    if event_count is None:
        raise BenchmarkError('perf report is missing the CPU event count')
    return {'data': str(data), 'report': str(report_path),
            'flat_report': str(flat_path), 'self_sample_percent': categories,
            'approx_user_cpu_nanos': int(event_count[1]),
            'sha256': hashlib.sha256(data.read_bytes()).hexdigest(), 'scope': 'timed read/GC phase only'}
