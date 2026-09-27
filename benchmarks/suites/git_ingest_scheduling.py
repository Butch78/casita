"""Native Git ingestion with controlled upload latency and resource-bound gates."""
from __future__ import annotations

import argparse
import hashlib
import itertools
import json
import os
import pathlib
import random
import subprocess
import tempfile

from benchmarks import cli
from benchmarks.suites import repository as common
from benchmarks.suites.metadata_collection import positive_csv
from benchmarks.suites.pack.catalog import parse_probe_binary

PROBE = 'git::repository::ingest_tests::benchmark_git_ingest_scheduling'
CORRECTNESS = 'exact independent Git inventory and complete closure; bounded active puts and bytes; oversized objects exclusive'
CARGO_ARGUMENTS = ('test', '--release', '--features', 'cli,git', '--lib', '--no-run', '--message-format=json')


def delays_csv(value):
    try:
        values = [int(part) for part in value.split(',')]
    except ValueError as error:
        raise argparse.ArgumentTypeError('expected distinct nonnegative delays') from error
    if len(values) != len(set(values)) or min(values) < 0:
        raise argparse.ArgumentTypeError('expected distinct nonnegative delays')
    return values


def parse_samples(stdout, files, concurrency, budget, delay, packed):
    try:
        rows = [json.loads(line.removeprefix('git_ingest_sample ')) for line in stdout.splitlines() if line.startswith('git_ingest_sample ')]
    except json.JSONDecodeError as error:
        raise common.BenchmarkError('invalid Git ingestion probe JSON') from error
    if len(rows) != 2 or 'test result: ok. 1 passed; 0 failed;' not in stdout:
        raise common.BenchmarkError('Git ingestion probe must execute one passing test and both imports')
    for row, operation in zip(rows, ('initial-import', 'incremental-import')):
        if (not isinstance(row, dict) or row.get('operation') != operation
                or row.get('files') != files or row.get('concurrency') != concurrency
                or row.get('max_buffered_bytes') != budget or row.get('delay_ms') != delay
                or row.get('packed') is not packed or row.get('correctness') != CORRECTNESS
                or not isinstance(row.get('root'), str) or not row['root'].startswith('git.view.v1:')
                or type(row.get('wall_nanos')) is not int or row['wall_nanos'] <= 0
                or type(row.get('peak_active')) is not int or not 1 <= row['peak_active'] <= concurrency
                or type(row.get('peak_bytes')) is not int or row['peak_bytes'] <= 0):
            raise common.BenchmarkError('wrong Git ingestion configuration or missing correctness gate')
    return rows


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', choices=('smoke', 'standard'), default='standard')
    parser.add_argument('--counts', type=positive_csv)
    parser.add_argument('--concurrency', type=positive_csv, default=[1, 16])
    parser.add_argument('--max-buffered-bytes', type=positive_csv, default=[65535, 65536, 65537, 67108864])
    parser.add_argument('--delays-ms', type=delays_csv, default=[0, 5])
    parser.add_argument('--layout', choices=('loose', 'packed', 'both'), default='both')
    parser.add_argument('--repetitions', type=int, default=3)
    parser.add_argument('--probe-binary', type=pathlib.Path)
    parser.add_argument('--no-build', action='store_true')
    parser.add_argument('--output', type=pathlib.Path, required=True)
    parser.add_argument('--report', type=pathlib.Path)
    parser.add_argument('--measurement-note', default='')
    args = parser.parse_args(argv)
    if args.repetitions < 1 or (args.no_build and args.probe_binary is None):
        parser.error('positive repetitions and a probe binary with --no-build are required')
    binary = args.probe_binary
    if binary is None:
        built = subprocess.run(['cargo', *CARGO_ARGUMENTS], cwd=cli.ROOT, capture_output=True, text=True)
        if built.returncode:
            raise common.BenchmarkError(built.stderr or built.stdout)
        binary = parse_probe_binary(built.stdout)
    binary = binary.resolve()
    with binary.open('rb') as handle:
        digest = hashlib.file_digest(handle, 'sha256').hexdigest()
    counts = args.counts or ([17] if args.profile == 'smoke' else [15, 16, 17, 64])
    layouts = [False, True] if args.layout == 'both' else [args.layout == 'packed']
    with tempfile.TemporaryDirectory(prefix='casita-git-scheduling-') as temporary:
        work = pathlib.Path(temporary)
        result = dict(schema_version=1, result_schema='casita.git-ingest-scheduling.v1', suite_id='native-git', complete=False,
            environment=common.environment_metadata(work), artifacts=[dict(path=str(binary), sha256=digest)], samples=[], processes=[],
            configuration=dict(files=counts, concurrency=args.concurrency, max_buffered_bytes=args.max_buffered_bytes, delays_ms=args.delays_ms,
                layouts=layouts, repetitions=args.repetitions, publication_batch_objects=7, measurement_note=args.measurement_note,
                timing='real initial/incremental Git imports into a memory payload store with controlled per-put delay; setup and independent inventory/closure validation excluded'))
        def save():
            common.write_atomic(args.output, json.dumps(result, indent=2) + '\n')
            if args.report:
                lines = ['# Git ingestion scheduling', '', f"Complete: {result['complete']}", '', args.measurement_note, '',
                    '| Files | Concurrency | Byte budget | Delay, ms | Packed | Operation | Seconds | Peak puts |', '|---:|---:|---:|---:|---|---|---:|---:|']
                for row in result['samples']:
                    lines.append(f"| {row['entries']} | {row['concurrency']} | {row['max_buffered_bytes']} | {row['delay_ms']} | {row['packed']} | {row['operation']} | {row['wall_seconds']:.6f} | {row['peak_active']} |")
                common.write_atomic(args.report, '\n'.join(lines) + '\n')
        save()
        roots = {}
        jobs = list(itertools.product(counts, args.concurrency, args.max_buffered_bytes, args.delays_ms, layouts, range(args.repetitions)))
        random.Random(1729).shuffle(jobs)
        try:
            for files, concurrency, budget, delay, packed, repetition in jobs:
                print(f'git-ingest-scheduling: files={files}, concurrency={concurrency}, bytes={budget}, delay={delay}, packed={packed}, repetition={repetition}', flush=True)
                env = {**os.environ, 'CASITA_GIT_FILES': str(files), 'CASITA_GIT_CONCURRENCY': str(concurrency), 'CASITA_GIT_BUFFERED_BYTES': str(budget),
                    'CASITA_GIT_DELAY_MS': str(delay), 'CASITA_GIT_PACKED': str(int(packed))}
                timing = common.measured_command(common.CommandSpec([[str(binary), PROBE, '--exact', '--ignored', '--nocapture']], work, env), work/'stdout', work/'stderr', check=False)
                stdout, stderr = (work/'stdout').read_text(), (work/'stderr').read_text()
                result['processes'].append(dict(**timing, files=files, concurrency=concurrency, max_buffered_bytes=budget, delay_ms=delay, packed=packed, repetition=repetition, stdout=stdout, stderr=stderr))
                if timing['exit_code'] != 0:
                    raise common.BenchmarkError(f'Git ingestion probe failed: {stdout}\n{stderr}')
                for row in parse_samples(stdout, files, concurrency, budget, delay, packed):
                    if row['root'] != roots.setdefault((files, row['operation']), row['root']):
                        raise common.BenchmarkError('concurrency or source packing changed the Git view')
                    result['samples'].append(dict(status='ok', implementation='casita', entries=files, repetition=repetition,
                        wall_seconds=row['wall_nanos']/1e9, max_rss_bytes=timing['max_rss_bytes'], **row))
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
