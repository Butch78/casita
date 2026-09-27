"""Compare immutable binaries using the permanent tar matrix and quiet-host gates."""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import pathlib
import platform
import subprocess
import statistics
import sys

from benchmarks.host_activity import QuietHost


def fingerprint(path):
    with path.open('rb') as handle:
        return hashlib.file_digest(handle, 'sha256').hexdigest()


def collect(directory):
    rows = []
    for path in sorted(directory.rglob('new/benchmark.json')):
        benchmark = json.loads(path.read_text())
        rows.append({'id': benchmark['full_id'],
                     'estimates_ns': json.loads(path.with_name('estimates.json').read_text()),
                     'samples': json.loads(path.with_name('sample.json').read_text())})
    expected = {f'tar_import_pipeline/{shape}/{limit}'
                for shape in ('small-0', 'small-1', 'small-15', 'small-16', 'small-17', 'small-256', 'large', 'mixed')
                for limit in (1, 16)}
    if len(rows) != len(expected) or {row['id'] for row in rows} != expected:
        raise ValueError('missing or unexpected cases in the 16-case tar matrix')
    return rows


def paired_comparison(runs, repetitions):
    """Summarize matched, accepted pairs; never mix in rejected measurements."""
    indexed = {(row['repetition'], row['variant']): row for row in runs}
    expected = {(repetition, variant) for repetition in range(repetitions)
                for variant in ('baseline', 'candidate')}
    if len(indexed) != len(runs) or set(indexed) != expected or any(
            row['status'] != 'accepted' for row in runs):
        raise ValueError('comparison requires complete accepted baseline/candidate pairs')
    pairs = {}
    for repetition in range(repetitions):
        cases = {}
        for variant in ('baseline', 'candidate'):
            rows = indexed[repetition, variant]['cases']
            cases[variant] = {row['id']: row['estimates_ns']['mean']['point_estimate']
                              for row in rows}
            if len(cases[variant]) != len(rows) or any(
                    not math.isfinite(value) or value <= 0 for value in cases[variant].values()):
                raise ValueError('invalid or duplicate case estimates')
        if not cases['baseline'] or cases['baseline'].keys() != cases['candidate'].keys():
            raise ValueError('paired case identities differ')
        if pairs and pairs.keys() != cases['baseline'].keys():
            raise ValueError('case identities changed between repetitions')
        for identifier, baseline in cases['baseline'].items():
            candidate = cases['candidate'][identifier]
            pairs.setdefault(identifier, []).append({
                'repetition': repetition, 'baseline_ns': baseline, 'candidate_ns': candidate,
                'candidate_over_baseline': candidate / baseline})
    return [{'id': identifier,
             'median_baseline_ns': statistics.median(pair['baseline_ns'] for pair in values),
             'median_candidate_ns': statistics.median(pair['candidate_ns'] for pair in values),
             'median_paired_time_ratio': statistics.median(pair['candidate_over_baseline'] for pair in values),
             'pairs': values} for identifier, values in sorted(pairs.items())]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=pathlib.Path)
    parser.add_argument('--baseline-binary', type=pathlib.Path,
                        help='alternate this baseline with --binary, reversing order each repetition')
    parser.add_argument('--output', required=True, type=pathlib.Path)
    parser.add_argument('--repetitions', type=int, default=4)
    parser.add_argument('--quiet-timeout', type=int, default=60)
    parser.add_argument('--max-external-cpu-percent', type=float, default=5,
                        help='maximum sampled background CPU across all logical CPUs (default: 5)')
    parser.add_argument('--allow-competing-builds', action='store_true',
                        help='record build activity without a separate veto; still enforce the CPU ceiling')
    parser.add_argument('--warm-up-time', type=float, default=1)
    parser.add_argument('--measurement-time', type=float, default=3)
    args = parser.parse_args(argv)
    if not math.isfinite(args.max_external_cpu_percent) or not 0 <= args.max_external_cpu_percent <= 100:
        parser.error('--max-external-cpu-percent must be finite and between 0 and 100')
    if min(args.repetitions, args.quiet_timeout, args.warm_up_time, args.measurement_time) <= 0:
        parser.error('counts and durations must be positive')
    binary = args.binary.resolve()
    original = fingerprint(binary)
    binaries = {'candidate': binary}
    if args.baseline_binary:
        binaries = {'baseline': args.baseline_binary.resolve(), **binaries}
    fingerprints = {label: fingerprint(path) for label, path in binaries.items()}
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    report = {'schema': 'casita.tar-paired.v2', 'binary': str(binary), 'binary_sha256': original,
              'binaries': {label: {'path': str(path), 'sha256': fingerprints[label]}
                           for label, path in binaries.items()},
              'environment': {'platform': platform.platform(), 'cpu_count': os.cpu_count(),
                              'cpuinfo': pathlib.Path('/proc/cpuinfo').read_text()},
              'configuration': {key: str(value) if isinstance(value, pathlib.Path) else value
                                for key, value in vars(args).items()},
              'host_policy': (f'10 quiet seconds before each matrix; <={args.max_external_cpu_percent:g}% sampled external CPU during the matrix; '
                              + ('competing processes recorded without a separate veto' if args.allow_competing_builds
                                 else 'no competing compiler/benchmark processes')),
              'runs': [], 'complete': False}
    def save():
        (output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    save()
    plan = [(repetition, variant) for repetition in range(args.repetitions)
            for variant in (list(binaries) if repetition % 2 == 0 else list(reversed(binaries)))]
    for repetition, variant in plan:
        run = {'repetition': repetition, 'variant': variant,
               'order': [1, 16] if repetition % 2 == 0 else [16, 1], 'status': 'pending'}
        report['runs'].append(run)
        directory = output / str(repetition)
        if args.baseline_binary:
            directory = directory / variant
        directory.mkdir(parents=True)
        environment = {**os.environ, 'CRITERION_HOME': str(directory / 'criterion')}
        for name in ('CASITA_TAR_REVERSE', 'CASITA_BENCH_PERF_CONTROL', 'CASITA_BENCH_PERF_ACK'):
            environment.pop(name, None)
        if repetition % 2:
            environment['CASITA_TAR_REVERSE'] = '1'
        command = [str(binaries[variant]), '--bench', '--warm-up-time', str(args.warm_up_time),
                   '--measurement-time', str(args.measurement_time), '--noplot']
        run['command'] = command
        print(f'tar matrix {repetition + 1}/{args.repetitions}, {variant}, order {run["order"]}', flush=True)
        monitor = QuietHost(timeout=args.quiet_timeout,
                            max_cpu_fraction=args.max_external_cpu_percent / 100,
                            allow_competing_builds=args.allow_competing_builds)
        # QuietHost can time out before entering its context. Preserve the
        # waiting samples too, so rejection has evidence rather than just an
        # error string. During measurement its monitor thread uses this method.
        run['activity_intervals'] = []
        original_sample = monitor.sample
        def recorded_sample():
            row = original_sample()
            run['activity_intervals'].append(row)
            return row
        monitor.sample = recorded_sample
        try:
            if any(fingerprint(path) != fingerprints[label] for label, path in binaries.items()):
                raise ValueError('benchmark binary changed')
            with monitor, (directory / 'run.log').open('w') as log:
                subprocess.run(command, env=environment, stdout=log, stderr=subprocess.STDOUT,
                               check=True, timeout=1800)
            run['host_activity'] = monitor.report()
            run['cases'] = collect(directory / 'criterion')
            if any(fingerprint(path) != fingerprints[label] for label, path in binaries.items()):
                raise ValueError('benchmark binary changed during measurement')
            run['status'] = 'accepted' if run['host_activity']['quiet'] else 'contended'
        except (Exception, KeyboardInterrupt) as error:
            run.update(status='failed', error=str(error) or type(error).__name__)
            if hasattr(monitor, 'wait_seconds'):
                run['host_activity'] = monitor.report()
            save()
            break
        save()
        if run['status'] != 'accepted':
            break
    report['complete'] = len(report['runs']) == len(plan) and all(run['status'] == 'accepted' for run in report['runs'])
    if report['complete'] and args.baseline_binary:
        try:
            report['comparison'] = paired_comparison(report['runs'], args.repetitions)
        except (KeyError, ValueError, TypeError) as error:
            report.update(complete=False, comparison_error=str(error))
    save()
    print(f'report: {output / "report.json"}; complete={report["complete"]}', flush=True)
    return 0 if report['complete'] else 1


if __name__ == '__main__':
    sys.exit(main())
