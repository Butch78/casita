"""Profile CPU inside the permanent tar-import cases, excluding benchmark validation."""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import platform
import re
import subprocess
import sys
import threading

from benchmarks import obrador_profile
from benchmarks.host_activity import QuietHost
from benchmarks.tar_compare import fingerprint


def summarize(flat):
    count = re.search(r'^# Event count \(approx\.\):\s+(\d+)', flat, re.MULTILINE)
    if count is None or int(count[1]) <= 0:
        raise ValueError('perf has no scoped CPU events')
    samples = re.search(r'^# Samples:\s+([0-9.,]+)([KMGT]?)', flat, re.MULTILINE)
    if samples is None:
        raise ValueError('perf report is missing its sample count')
    approximate_samples = float(samples[1].replace(',', '')) * 1000 ** (' KMGT'.index(samples[2]) if samples[2] else 0)
    if approximate_samples < 100:
        raise ValueError('fewer than 100 scoped CPU samples')
    symbols = []
    hash_threads = {}
    categories = dict.fromkeys(('blake3', 'fastcdc', 'zstd', 'allocation', 'metadata', 'runtime', 'other'), 0.0)
    for percent, thread, library, symbol in re.findall(r'^\s+(\d+\.\d+)%\s+(\S+)\s+(\S+)\s+\[\.\]\s+(.*)$', flat, re.MULTILINE):
        percent = float(percent)
        lower = symbol.lower()
        category = ('blake3' if 'blake3' in lower else 'fastcdc' if 'fastcdc' in lower
                    else 'zstd' if any(name in lower for name in ('zstd', 'huf_', 'fse_'))
                    else 'allocation' if any(name in lower for name in ('malloc', 'realloc', '__rust_alloc', '__rust_dealloc', 'cfree', '__libc_free'))
                    else 'metadata' if 'casita::metadata' in lower
                    else 'runtime' if any(name in lower for name in ('tokio::', 'futures_', 'futures::'))
                    else 'other')
        categories[category] += percent
        if category == 'blake3':
            hash_threads[thread] = hash_threads.get(thread, 0) + percent
        symbols.append({'self_percent': percent, 'thread_name': thread, 'shared_object': library, 'symbol': symbol})
    if not symbols:
        raise ValueError('perf report has no user-space symbols')
    return {'approx_user_cpu_ns': int(count[1]), 'approx_samples': approximate_samples,
            'self_percent_by_symbol_category': categories, 'blake3_self_percent_by_thread_name': hash_threads,
            'top_symbols': sorted(symbols, key=lambda row: row['self_percent'], reverse=True)[:40]}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=pathlib.Path)
    parser.add_argument('--perf', required=True, type=pathlib.Path)
    parser.add_argument('--output', required=True, type=pathlib.Path)
    parser.add_argument('--seconds', type=int, default=10)
    args = parser.parse_args(argv)
    if args.seconds < 1:
        parser.error('--seconds must be positive')
    binary, perf, output = args.binary.resolve(), args.perf.resolve(), args.output.resolve()
    original = fingerprint(binary)
    output.mkdir(parents=True, exist_ok=False)
    report = {'schema': 'casita.tar-profile.v1', 'binary': str(binary), 'binary_sha256': original,
              'perf': str(perf), 'perf_version': subprocess.check_output([str(perf), '--version'], text=True).strip(),
              'scope': 'request.import only, including its verification/publication; excludes fixture/repository setup and benchmark readback; CPU attribution under recorded host load, not throughput',
              'environment': {'platform': platform.platform(), 'cpu_count': os.cpu_count()},
              'source_sha256': {path: fingerprint(pathlib.Path(path)) for path in
                                ('crates/casita/benches/tar_import.rs', 'crates/casita/benches/bench_util/perf.rs', 'benchmarks/tar_profile.py')},
              'runs': [], 'complete': False}
    def save():
        (output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    save()
    for shape in ('small-256', 'large', 'mixed'):
        for concurrency in (1, 16):
            case = f'tar_import_pipeline/{shape}/{concurrency}'
            log = output / f'{shape}-{concurrency}.log'
            base_command = [str(binary), '--bench', '--exact', case, '--profile-time', str(args.seconds)]
            command, environment = obrador_profile.command(perf, base_command, log)
            environment.pop('CASITA_TAR_REVERSE', None)
            environment['CRITERION_HOME'] = str(output / 'criterion')
            row = {'case': case, 'command': command, 'status': 'pending'}
            report['runs'].append(row)
            monitor = QuietHost()
            activity = []
            def sample():
                while not monitor.stop.is_set():
                    interval = monitor.sample()
                    if interval['interval_seconds'] >= 0.5:
                        activity.append(interval)
            thread = threading.Thread(target=sample, daemon=True)
            print(f'profiling {case}', flush=True)
            try:
                if fingerprint(binary) != original:
                    raise ValueError('benchmark binary changed')
                thread.start()
                try:
                    with log.open('w') as handle:
                        subprocess.run(command, env=environment, stdout=handle, stderr=subprocess.STDOUT,
                                       check=True, timeout=600)
                finally:
                    monitor.stop.set()
                    thread.join()
                    row['host_activity'] = activity
                if 'Scoped tar import profiling enabled' not in log.read_text():
                    raise ValueError('benchmark did not confirm scoped profiling')
                data = log.with_suffix('.perf.data')
                flat = log.with_suffix('.perf-flat.txt')
                with flat.open('w') as handle:
                    subprocess.run([str(perf), 'report', '--stdio', '--no-children', '--call-graph', 'none',
                                    '--percent-limit', '0', '-i', str(data)], stdout=handle, stderr=subprocess.STDOUT, check=True)
                row.update(summarize(flat.read_text()))
                row.update(data_sha256=fingerprint(data), flat_report=str(flat), status='passed')
                if fingerprint(binary) != original:
                    raise ValueError('benchmark binary changed during profiling')
            except (Exception, KeyboardInterrupt) as error:
                row.update(status='failed', error=str(error) or type(error).__name__)
                save()
                return 1
            save()
    report['complete'] = True
    save()
    print(f'report: {output / "report.json"}', flush=True)
    return 0


if __name__ == '__main__':
    sys.exit(main())
