"""Permanent local journal checkpoint, group-size, and migration-space boundaries."""
from __future__ import annotations
import argparse
import hashlib
import json
import os
import pathlib
import random
import subprocess
import tempfile
from benchmarks import cli
from benchmarks.suites import repository as common
from benchmarks.suites.durable_ledger import METRICS
from benchmarks.suites.metadata_collection import CARGO_ARGUMENTS
from benchmarks.suites.pack.catalog import parse_probe_binary

PROBE = 'metadata::pins::persistent::journal::tests::benchmark_journal_boundaries'
CORRECTNESS = 'exact replay, bounded groups, exact checkpoint and migration boundaries, no leaked protection'
CASES = {'checkpoint-operations': (255, 256, 257), 'checkpoint-bytes': (3, 4, 5),
         'checkpoint-record-bytes': (1044480, 1048576, 1052672),
         'group-size': (1, 2, 63, 64, 65), 'migration-space': (0, 1)}


def parse_sample(stdout, boundary, position):
    try:
        cases = [json.loads(line.removeprefix('ledger_boundary_sample ')) for line in stdout.splitlines()
                 if line.startswith('ledger_boundary_sample ')]
    except json.JSONDecodeError as error:
        raise common.BenchmarkError('invalid ledger boundary JSON') from error
    if 'test result: ok. 1 passed; 0 failed;' not in stdout or len(cases) != 1:
        raise common.BenchmarkError('ledger boundary must execute exactly one passing test')
    case = cases[0]
    if (not isinstance(case, dict) or case.get('boundary') != boundary or case.get('position') != position
            or case.get('correctness') != CORRECTNESS or type(case.get('nanos')) is not int or case['nanos'] < 0):
        raise common.BenchmarkError('wrong boundary configuration or missing correctness gate')
    metrics = case.get('metrics', {})
    if not isinstance(metrics, dict) or any(type(metrics.get(key)) is not int or metrics[key] < 0 for key in METRICS):
        raise common.BenchmarkError('missing boundary durability metrics')
    if boundary not in CASES or position not in CASES[boundary]:
        raise common.BenchmarkError('unknown ledger boundary')
    if case.get('journal_encoding') not in (None, 'full-record-v1', 'resource-additions-v2'):
        raise common.BenchmarkError('unknown journal encoding')
    if boundary == 'checkpoint-record-bytes':
        if case.get('journal_encoding') == 'resource-additions-v2':
            expected = dict(checkpoints=0, journal_frames=3, journal_syncs=3, journal_bytes=3*4232)
        else:
            oversized = position >= 1048576
            expected = dict(checkpoints=3 if oversized else 1, journal_frames=0 if oversized else 2,
                            journal_syncs=6 if oversized else 4)
    elif boundary.startswith('checkpoint-'):
        checkpoint = position == (257 if boundary == 'checkpoint-operations' else 4)
        expected = dict(checkpoints=int(checkpoint), journal_frames=int(not checkpoint), journal_syncs=2 if checkpoint else 1)
    elif boundary == 'group-size':
        groups = (position + 63) // 64
        expected = dict(groups=groups, operations=position, journal_frames=groups, journal_syncs=groups, max_group=min(position, 64))
    else:
        expected = dict(checkpoints=position, replacement_updates=int(position == 0))
    if any(metrics[key] != value for key, value in expected.items()):
        raise common.BenchmarkError('journal boundary correctness metrics failed')
    return case


def save(args, result):
    common.write_atomic(args.output, json.dumps(result, indent=2) + '\n')
    lines = ['# Local ledger boundaries', '', f"Complete: {result['complete']}", '',
             'Every profile covers both sides of each limit. Setup and cold replay checks are outside timing.',
             'Groups invoke the production batch executor directly, excluding asynchronous queue scheduling.',
             'Migration space is simulated allocation denial, not a physically full device.',
             'Byte cases add 256 KiB catalog records; the fourth frame crosses the 1 MiB journal window.', '',
             'Record-byte cases time three tiny protection edits on one retained catalog near 1 MiB; V2 frames encode only additions.', '',
             '| Boundary | Position | Repetition | ms |', '|---|---:|---:|---:|']
    if 'error' in result:
        lines += [result['error'], '']
    for sample in result['samples']:
        lines.append(f"| {sample['operation']} | {sample['entries']} | {sample['repetition']} | {sample['wall_seconds'] * 1000:.3f} |")
    common.write_atomic(args.report or args.output.with_suffix('.md'), '\n'.join(lines) + '\n')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', choices=('smoke', 'standard'), default='standard')
    parser.add_argument('--repetitions', type=int, default=3)
    parser.add_argument('--probe-binary', type=pathlib.Path)
    parser.add_argument('--no-build', action='store_true')
    parser.add_argument('--output', type=pathlib.Path, required=True)
    parser.add_argument('--report', type=pathlib.Path)
    args = parser.parse_args(argv)
    if args.repetitions < 1:
        parser.error('repetitions must be positive')
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
    with tempfile.TemporaryDirectory(prefix='casita-ledger-boundaries-') as temporary:
        work = pathlib.Path(temporary)
        result = dict(schema_version=1, result_schema='casita.ledger-boundaries.v1', suite_id='state-and-publication', complete=False,
                      environment=common.environment_metadata(work), configuration=dict(profile=args.profile, cases=CASES, repetitions=args.repetitions),
                      artifacts=[dict(path=str(binary), sha256=digest)], samples=[], processes=[])
        save(args, result)
        schedule = [(rep, boundary, position) for rep in range(1, args.repetitions + 1) for boundary, positions in CASES.items() for position in positions]
        random.Random(0xCA517A).shuffle(schedule)
        try:
            for repetition, boundary, position in schedule:
                print(f'ledger-boundaries: {boundary} {position}, repetition {repetition}', flush=True)
                env = {**os.environ, 'CASITA_LEDGER_BOUNDARY': boundary, 'CASITA_LEDGER_POSITION': str(position)}
                stdout, stderr = work / 'stdout', work / 'stderr'
                timing = common.measured_command(common.CommandSpec([[str(binary), PROBE, '--exact', '--ignored', '--nocapture']], work, env), stdout, stderr, check=False)
                captured = stdout.read_text()
                result['processes'].append({**timing, 'boundary': boundary, 'position': position, 'repetition': repetition, 'stdout': captured, 'stderr': stderr.read_text()})
                if timing['exit_code']:
                    raise common.BenchmarkError(f'ledger boundary failed: {captured}\n{stderr.read_text()}')
                case = parse_sample(captured, boundary, position)
                result['samples'].append(dict(status='ok', operation=boundary, entries=position, repetition=repetition,
                    wall_seconds=case['nanos'] / 1e9, max_rss_bytes=timing['max_rss_bytes'], metrics=case['metrics'],
                    journal_encoding=case.get('journal_encoding', 'full-record-v1'), correctness=CORRECTNESS))
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
