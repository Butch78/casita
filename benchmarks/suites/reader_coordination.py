"""Local process reader coordination, active inventory scaling, and revision reservation boundary."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import subprocess
import tempfile

from benchmarks import cli
from benchmarks.suites import repository as common
from benchmarks.suites.metadata_collection import CARGO_ARGUMENTS, positive_csv
from benchmarks.suites.pack.catalog import parse_probe_binary

PROBE = 'metadata::pins::persistent::readers::tests::benchmark_reader_coordination'
CORRECTNESS = 'durable write protection preserved, no leaked readers, stale and protected deletion claims rejected, exact reservation boundary'
PHASES = {'cold-register': 1, 'warm-register': 0, 'warm-protect': 0, 'warm-release': 0,
          'last-reserved-register': 0, 'reservation-rollover-protect': 1, 'renewed-release': 0}


def parse_sample(stdout, active, iterations):
    try:
        cases = [json.loads(line.removeprefix('reader_coordination_sample '))
                 for line in stdout.splitlines() if line.startswith('reader_coordination_sample ')]
    except json.JSONDecodeError as error:
        raise common.BenchmarkError('invalid reader coordination JSON') from error
    if 'test result: ok. 1 passed; 0 failed;' not in stdout or len(cases) != 1:
        raise common.BenchmarkError('reader coordination probe must execute exactly one passing test')
    case = cases[0]
    if (not isinstance(case, dict) or case.get('active_readers') != active
            or case.get('iterations') != iterations or case.get('reservation') != 65536
            or case.get('correctness') != CORRECTNESS):
        raise common.BenchmarkError('wrong reader coordination configuration or correctness gate')
    samples = case.get('samples')
    expected = ['cold-register', *(['warm-register', 'warm-protect', 'warm-release'] * iterations),
                'last-reserved-register', 'reservation-rollover-protect', 'renewed-release']
    if not isinstance(samples, list) or len(samples) != len(expected):
        raise common.BenchmarkError('missing reader coordination samples')
    for sample, phase in zip(samples, expected):
        if (not isinstance(sample, dict) or sample.get('phase') != phase
                or type(sample.get('nanos')) is not int or sample['nanos'] < 0
                or type(sample.get('durable_changed')) is not int or sample['durable_changed'] != PHASES[phase]):
            raise common.BenchmarkError('invalid reader coordination timing or durability gate')
    return case


def save(args, result):
    common.write_atomic(args.output, json.dumps(result, indent=2) + '\n')
    lines = ['# Local reader coordination', '', f"Complete: {result['complete']}", '',
             'Cold measures first owner registration. Warm phases run with the specified other readers active.',
             'Boundary setup moves the real counter to its last reserved revision outside timing; rollover uses the production reserve path.',
             'Timing includes the blocking worker and atomic inventory replacement. Durability gates compare the ledger bytes outside timing.',
             'Each warm value is the mean per process. Raw iterations are retained. OS caches are not flushed.', '',
             '| Other readers | Phase | Repetition | Mean ms | Durable ledger changed |', '|---:|---|---:|---:|---:|']
    if 'error' in result:
        lines += [result['error'], '']
    for sample in result['samples']:
        lines.append(f"| {sample['readers']} | {sample['operation']} | {sample['repetition']} | {sample['wall_seconds'] * 1000:.3f} | {sample['metrics']['durable_revision_changed']} |")
    common.write_atomic(args.report or args.output.with_suffix('.md'), '\n'.join(lines) + '\n')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', choices=('smoke', 'standard'), default='standard')
    parser.add_argument('--counts', type=positive_csv, default=[1, 64])
    parser.add_argument('--iterations', type=int)
    parser.add_argument('--repetitions', type=int, default=3)
    parser.add_argument('--probe-binary', type=pathlib.Path)
    parser.add_argument('--no-build', action='store_true')
    parser.add_argument('--output', type=pathlib.Path, required=True)
    parser.add_argument('--report', type=pathlib.Path)
    args = parser.parse_args(argv)
    iterations = args.iterations if args.iterations is not None else (8 if args.profile == 'smoke' else 128)
    if args.repetitions < 1 or iterations < 1:
        parser.error('iterations and repetitions must be positive')
    if args.no_build and args.probe_binary is None:
        parser.error('--no-build requires --probe-binary')
    binary = args.probe_binary
    if binary is None:
        built = subprocess.run(['cargo', *CARGO_ARGUMENTS], cwd=cli.ROOT, capture_output=True, text=True)
        if built.returncode:
            raise common.BenchmarkError(built.stderr or built.stdout)
        binary = parse_probe_binary(built.stdout)
    binary = binary.resolve()
    with binary.open('rb') as handle:
        digest = hashlib.file_digest(handle, 'sha256').hexdigest()
    with tempfile.TemporaryDirectory(prefix='casita-reader-coordination-') as temporary:
        work = pathlib.Path(temporary)
        result = dict(schema_version=1, result_schema='casita.reader-coordination.v1', suite_id='state-and-publication',
                      complete=False, environment=common.environment_metadata(work),
                      configuration=dict(profile=args.profile, counts=args.counts, iterations=iterations, repetitions=args.repetitions),
                      artifacts=[dict(path=str(binary), sha256=digest)], samples=[], processes=[])
        save(args, result)
        try:
            for repetition in range(1, args.repetitions + 1):
                for active in args.counts:
                    print(f'reader-coordination: {active} active, repetition {repetition}', flush=True)
                    env = {**os.environ, 'CASITA_ACTIVE_READERS': str(active), 'CASITA_READER_ITERATIONS': str(iterations)}
                    stdout, stderr = work / 'stdout', work / 'stderr'
                    timing = common.measured_command(common.CommandSpec(
                        [[str(binary), PROBE, '--exact', '--ignored', '--nocapture']], work, env), stdout, stderr, check=False)
                    captured = stdout.read_text()
                    result['processes'].append({**timing, 'active_readers': active, 'repetition': repetition,
                                                'stdout': captured, 'stderr': stderr.read_text()})
                    if timing['exit_code']:
                        raise common.BenchmarkError(f'reader coordination probe failed: {captured}\n{stderr.read_text()}')
                    case = parse_sample(captured, active, iterations)
                    for phase, changed in PHASES.items():
                        nanos = [sample['nanos'] for sample in case['samples'] if sample['phase'] == phase]
                        result['samples'].append(dict(status='ok', operation=phase, readers=active, repetition=repetition,
                            wall_seconds=sum(nanos) / len(nanos) / 1e9, max_rss_bytes=timing['max_rss_bytes'],
                            metrics=dict(durable_revision_changed=changed), correctness=CORRECTNESS))
                    save(args, result)
        except Exception as error:
            result['error'] = str(error)
            save(args, result)
            raise
        result['complete'] = True
        save(args, result)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
