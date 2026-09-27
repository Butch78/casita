"""Durable protection cost as one staging pin grows across the journal window."""
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

PROBE = 'metadata::pins::persistent::benchmarks::benchmark_pin_growth'
CORRECTNESS = 'exact replayed resources; protected deletion rejected; release permits deletion; no leaked pins or claims'


def parse_sample(stdout, count, iterations):
    try:
        rows = [json.loads(line.removeprefix('pin_growth_sample ')) for line in stdout.splitlines() if line.startswith('pin_growth_sample ')]
    except json.JSONDecodeError as error:
        raise common.BenchmarkError('invalid pin growth JSON') from error
    if len(rows) != 1 or 'test result: ok. 1 passed; 0 failed;' not in stdout:
        raise common.BenchmarkError('pin growth requires one passing probe')
    row = rows[0]
    if row.get('resources') != count or row.get('iterations') != iterations or row.get('correctness') != CORRECTNESS:
        raise common.BenchmarkError('wrong pin growth configuration or correctness gate')
    nanos = row.get('nanos')
    if not isinstance(nanos, list) or len(nanos) != iterations or any(type(n) is not int or n <= 0 for n in nanos):
        raise common.BenchmarkError('missing pin growth timings')
    metrics = row.get('metrics', {})
    if (metrics.get('operations') != iterations or metrics.get('journal_syncs', 0) < iterations
            or metrics.get('replacement_updates') != 0):
        raise common.BenchmarkError('missing durable protection barriers')
    return row


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', choices=('smoke', 'standard'), default='standard')
    parser.add_argument('--counts', type=positive_csv)
    parser.add_argument('--iterations', type=int)
    parser.add_argument('--repetitions', type=int, default=3)
    parser.add_argument('--probe-binary', type=pathlib.Path)
    parser.add_argument('--no-build', action='store_true')
    parser.add_argument('--output', type=pathlib.Path, required=True)
    parser.add_argument('--report', type=pathlib.Path)
    args = parser.parse_args(argv)
    counts = args.counts or [1, 8192, 16384]
    # Standard crosses multiple operation/byte checkpoints, including for pins
    # already larger than the window. Smoke retains both sides of that cliff.
    iterations = args.iterations if args.iterations is not None else (2 if args.profile == 'smoke' else 1024)
    if min(iterations, args.repetitions) < 1 or (args.no_build and args.probe_binary is None):
        parser.error('positive iterations/repetitions and a binary for --no-build are required')
    binary = args.probe_binary
    if binary is None:
        built = subprocess.run(['cargo', *CARGO_ARGUMENTS], cwd=cli.ROOT, capture_output=True, text=True)
        if built.returncode:
            raise common.BenchmarkError(built.stderr or built.stdout)
        binary = parse_probe_binary(built.stdout)
    binary = binary.resolve()
    with binary.open('rb') as handle:
        digest = hashlib.file_digest(handle, 'sha256').hexdigest()
    with tempfile.TemporaryDirectory(prefix='casita-pin-growth-') as temporary:
        work = pathlib.Path(temporary)
        result = dict(schema_version=1, result_schema='casita.pin-growth.v1', suite_id='state-and-publication', complete=False,
                      environment=common.environment_metadata(work), artifacts=[dict(path=str(binary), sha256=digest)],
                      configuration=dict(profile=args.profile, counts=counts, iterations=iterations, repetitions=args.repetitions), samples=[], processes=[])
        def save():
            common.write_atomic(args.output, json.dumps(result, indent=2)+'\n')
            lines = ['# Growing staging pin', '', f"Complete: {result['complete']}", '',
                     'Seed and replay/audits excluded from protection time. Sequential durable additions to one pin; warm cache.', '',
                     '| Resources | Repetition | Mean protection, ms | Journal bytes | Checkpoints |', '|---:|---:|---:|---:|---:|']
            for row in result['samples']:
                lines.append(f"| {row['entries']} | {row['repetition']} | {row['wall_seconds']*1000:.3f} | {row['metrics']['journal_bytes']} | {row['metrics']['checkpoints']} |")
            common.write_atomic(args.report or args.output.with_suffix('.md'), '\n'.join(lines)+'\n')
        save()
        try:
            for rep in range(args.repetitions):
                for count in counts:
                    print(f'pin-growth: {count} resources, repetition {rep}', flush=True)
                    env = {**os.environ, 'CASITA_PIN_RESOURCES': str(count), 'CASITA_PIN_ITERATIONS': str(iterations)}
                    timing = common.measured_command(common.CommandSpec([[str(binary), PROBE, '--exact', '--ignored', '--nocapture']], work, env), work/'stdout', work/'stderr', check=False)
                    stdout, stderr = (work/'stdout').read_text(), (work/'stderr').read_text()
                    result['processes'].append(dict(resources=count, repetition=rep, stdout=stdout, stderr=stderr, **timing))
                    if timing['exit_code']:
                        raise common.BenchmarkError(f'pin growth failed: {stdout}\n{stderr}')
                    row = parse_sample(stdout, count, iterations)
                    result['samples'].append(dict(status='ok', operation='protect', implementation='casita', entries=count, repetition=rep,
                                                 wall_seconds=sum(row['nanos'])/iterations/1e9, max_rss_bytes=timing['max_rss_bytes'], **row))
                    save()
            result['complete'] = True
        except Exception as error:
            result['error'] = str(error)
            raise
        finally:
            save()
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
