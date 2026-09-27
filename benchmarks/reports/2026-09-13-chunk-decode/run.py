"""Compare the permanent chunk decoder cases twice, reversing variant order.

Usage: python3 run.py IMMUTABLE_OPTIMIZATION_BINARY NEW_OUTPUT_DIRECTORY
"""
import itertools
import json
import os
import pathlib
import statistics
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
from benchmarks.host_activity import QuietHost
from benchmarks.tar_compare import fingerprint


def main(binary, output):
    output.mkdir(parents=True, exist_ok=False)
    original = fingerprint(binary)
    expected = {f'chunk_decompression/{frame}/{flavor}/{size}/{variant}'
                for frame, flavor, size, variant in itertools.product(
                    ('sized', 'unsized', 'concatenated'), ('random', 'text'),
                    (1024, 65535, 65536, 65537, 131071, 131072, 131073, 262144, 524288),
                    ('streaming', 'reused'))}
    report = {'schema': 'casita.chunk-decode-comparison.v1', 'binary': str(binary),
              'binary_sha256': original, 'runs': [], 'complete': False,
              'policy': '30% sampled external CPU; build activity recorded without a separate veto; two matrices with reversed variant order',
              'source_sha256': {p: fingerprint(ROOT / p) for p in ('benches/optimization.rs', 'src/compression.rs', 'src/blob/chunked/manifest.rs')}}
    def save():
        (output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    for repetition in range(2):
        directory = output / str(repetition)
        directory.mkdir()
        env = {**os.environ, 'CRITERION_HOME': str(directory / 'criterion')}
        env.pop('CASITA_CHUNK_DECODE_REVERSE', None)
        if repetition:
            env['CASITA_CHUNK_DECODE_REVERSE'] = '1'
        command = [str(binary), '--bench', 'chunk_decompression', '--warm-up-time', '0.1',
                   '--measurement-time', '0.3', '--noplot']
        run = {'repetition': repetition, 'reverse': bool(repetition), 'command': command, 'status': 'pending'}
        report['runs'].append(run)
        monitor = QuietHost(timeout=60, max_cpu_fraction=0.3, allow_competing_builds=True)
        run['activity_intervals'] = []
        sample = monitor.sample
        def recorded_sample():
            row = sample()
            run['activity_intervals'].append(row)
            return row
        monitor.sample = recorded_sample
        save()
        print('Decoder matrix', repetition + 1, flush=True)
        try:
            if fingerprint(binary) != original:
                raise ValueError('benchmark executable changed')
            with monitor, (directory / 'run.log').open('w') as log:
                subprocess.run(command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT,
                               check=True, timeout=600)
            run['host_activity'] = monitor.report()
            cases = []
            for path in sorted((directory / 'criterion').rglob('new/benchmark.json')):
                identifier = json.loads(path.read_text())['full_id']
                cases.append({'id': identifier, 'estimates': json.loads(path.with_name('estimates.json').read_text()),
                              'samples': json.loads(path.with_name('sample.json').read_text())})
            if len(cases) != len(expected) or {row['id'] for row in cases} != expected:
                raise ValueError('incomplete decoder case matrix')
            run['cases'] = cases
            run['preflight'] = [json.loads(line) for line in (directory / 'run.log').read_text().splitlines()
                                if line.startswith('{') and json.loads(line).get('schema') == 'casita.chunk-decompression.v1']
            if len(run['preflight']) != len(expected) or fingerprint(binary) != original:
                raise ValueError('missing preflight or changed binary')
            run['status'] = 'accepted' if run['host_activity']['quiet'] else 'contended'
        except (Exception, KeyboardInterrupt) as error:
            run.update(status='failed', error=str(error) or type(error).__name__)
            if hasattr(monitor, 'wait_seconds'):
                run['host_activity'] = monitor.report()
            save()
            break
        save()
    report['complete'] = len(report['runs']) == 2 and all(r['status'] == 'accepted' for r in report['runs'])
    if report['complete']:
        indexed = [{row['id']: row['estimates']['mean']['point_estimate'] for row in run['cases']}
                   for run in report['runs']]
        report['comparison'] = [{
            'case': identifier.rsplit('/', 1)[0],
            'median_reused_over_streaming': statistics.median(
                values[identifier.replace('/streaming', '/reused')] / values[identifier] for values in indexed),
            'streaming_ns': [values[identifier] for values in indexed],
            'reused_ns': [values[identifier.replace('/streaming', '/reused')] for values in indexed],
        } for identifier in sorted(expected) if identifier.endswith('/streaming')]
    save()
    print('Complete:', report['complete'], flush=True)
    return 0 if report['complete'] else 1


if __name__ == '__main__':
    raise SystemExit(main(pathlib.Path(sys.argv[1]).resolve(), pathlib.Path(sys.argv[2]).resolve()))
