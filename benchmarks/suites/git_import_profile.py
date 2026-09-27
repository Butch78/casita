"""Stage-level native Git import profiling with the permanent git-scale workloads."""
from __future__ import annotations

import argparse
import base64
import dataclasses
import hashlib
import json
import os
import pathlib
import subprocess
import tempfile

from benchmarks import cli
from benchmarks.suites import git as corpus
from benchmarks.suites import repository as common
from benchmarks.suites.pack.catalog import parse_probe_binary

PROBE = 'git::repository::import_profile::benchmark_git_import_profile'
PHASES = ['mutation_start', 'existing_view', 'source_header', 'source_decode', 'stage_poll', 'verify_sum', 'upload_sum', 'checkpoint_drain', 'checkpoint_publish', 'pack_cache', 'root_publish', 'compact']
CORRECTNESS = 'exact independent Git inventory and ref; persisted view identity; complete verified closure'
CARGO_ARGUMENTS = ('test', '--release', '--features', 'cli,git', '--lib', '--no-run', '--message-format=json')


def shapes_csv(value):
    names = value.split(',')
    if len(names) != len(set(names)) or any(name not in corpus.SCALES['standard'] for name in names):
        raise argparse.ArgumentTypeError('expected distinct git-scale shape names')
    return names


def object_key(kind, oid):
    return f'git.sha1.{kind}.v1:' + base64.urlsafe_b64encode(bytes.fromhex(oid)).decode().rstrip('=')


def inventory(source, output):
    def git(*args):
        return corpus.git('git', source, *args)
    ids = common.run_checked(git('rev-list', '--objects', '--all'), env=corpus.git_env())
    result = subprocess.run(git('cat-file', '--batch-check'), input=''.join(line.split()[0]+'\n' for line in ids.splitlines()),
                            env=corpus.git_env(), capture_output=True, text=True, check=True)
    records = [line.split() for line in result.stdout.splitlines()]
    keys = sorted(object_key(kind, oid) for oid, kind, size in records)
    common.write_atomic(output, '\n'.join(keys)+'\n')
    tip = common.run_checked(git('rev-parse', 'refs/heads/main'), env=corpus.git_env()).strip()
    return dict(objects=len(keys), tip=object_key('commit', tip), logical_bytes=sum(int(row[2]) for row in records),
                inventory_sha256=hashlib.sha256(output.read_bytes()).hexdigest(), source=corpus.source_metrics('git', source))


def prepare_fixture(root, profile, shape):
    """Reusable immutable sources, generated only outside timed processes."""
    directory = root / profile / shape
    if (directory/'fixture.json').exists():
        result = json.loads((directory/'fixture.json').read_text())
        if result['scale'] != dataclasses.asdict(corpus.SCALES[profile][shape]):
            raise common.BenchmarkError('cached fixture has a different scale')
        return directory, result
    directory.mkdir(parents=True, exist_ok=False)
    scale = corpus.SCALES[profile][shape]
    print(f'preparing {profile}/{shape}: {scale.commits} commits, {scale.files} files, {scale.logical_blob_bytes} logical blob bytes', flush=True)
    base = directory/'base.git'
    corpus.create_source('git', base, scale)
    # Repack explicitly to exercise actual delta-chain decoding in delta-heavy
    # history, not merely a source containing similar loose objects.
    common.run_checked(corpus.git('git', base, 'repack', '-adf', '--window=16'), env=corpus.git_env())
    incremental = directory/'incremental.git'
    common.run_checked(['git', 'clone', '--quiet', '--mirror', '--no-local', str(base), str(incremental)], env=corpus.git_env())
    corpus.append_history('git', incremental, scale, first_commit=scale.commits, commit_count=scale.incremental_commits)
    common.run_checked(corpus.git('git', incremental, 'repack', '-adf', '--window=16'), env=corpus.git_env())
    result = dict(profile=profile, shape=shape, scale=dataclasses.asdict(scale), sources={})
    for name in ('base', 'incremental'):
        source = directory/f'{name}.git'
        common.run_checked(corpus.git('git', source, 'fsck', '--full', '--strict'), env=corpus.git_env())
        result['sources'][name] = inventory(source, directory/f'{name}.inventory')
    common.write_atomic(directory/'fixture.json', json.dumps(result, indent=2)+'\n')
    return directory, result


def parse_sample(stdout, operation, expected):
    try:
        rows = [json.loads(line.removeprefix('git_import_profile ')) for line in stdout.splitlines() if line.startswith('git_import_profile ')]
    except json.JSONDecodeError as error:
        raise common.BenchmarkError('invalid profile JSON') from error
    if len(rows) != 1 or 'test result: ok. 1 passed; 0 failed;' not in stdout:
        raise common.BenchmarkError('profile must run exactly one passing probe')
    row = rows[0]
    if (row.get('operation') != operation or row.get('objects') != expected['objects'] or row.get('correctness') != CORRECTNESS
            or row.get('concurrency') != 16 or row.get('max_buffered_bytes') != 67108864 or row.get('phase_names') != PHASES
            or type(row.get('wall_nanos')) is not int or row['wall_nanos'] <= 0):
        raise common.BenchmarkError('wrong profile configuration or missing correctness gate')
    profile = row.get('profile', {})
    for name in ('calls', 'nanos'):
        values = profile.get(name)
        if not isinstance(values, list) or len(values) != len(PHASES) or any(type(v) is not int or v < 0 for v in values):
            raise common.BenchmarkError('missing phase counters')
    if type(profile.get('peak_active')) is not int or not 0 <= profile['peak_active'] <= 16:
        raise common.BenchmarkError('wrong active-object bound')
    for name in ('decoded_objects', 'decoded_bytes', 'peak_buffered_bytes'):
        if type(profile.get(name)) is not int or profile[name] < 0:
            raise common.BenchmarkError('missing decoded-object counters')
    if (profile['decoded_objects'] > expected['objects']
            or (operation == 'initial-import' and profile['decoded_objects'] != expected['objects'])
            or (operation == 'unchanged-import' and profile['decoded_objects'] != 0)
            or not isinstance(row.get('root'), str) or not row['root']):
        raise common.BenchmarkError('wrong traversal or missing persisted root')
    return row


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', choices=('smoke', 'standard'), default='standard')
    parser.add_argument('--shapes', type=shapes_csv, default=['many-objects', 'delta-heavy', 'wide-tree'])
    parser.add_argument('--repetitions', type=int, default=1)
    parser.add_argument('--fixture-root', type=pathlib.Path, help='Reuse or create git-scale sources below this directory')
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
        binary_hash = hashlib.file_digest(handle, 'sha256').hexdigest()
    with tempfile.TemporaryDirectory(prefix='casita-git-profile-') as temporary:
        work = pathlib.Path(temporary)
        result = dict(schema_version=1, result_schema='casita.git-import-profile.v1', suite_id='native-git', complete=False,
            environment=common.environment_metadata(work), artifacts=[dict(path=str(binary), sha256=binary_hash)], samples=[], processes=[], corpora={},
            configuration=dict(profile=args.profile, shapes=args.shapes, repetitions=args.repetitions, concurrency=16, max_buffered_bytes=67108864,
                max_cached_pack_bytes=0, measurement_note=args.measurement_note,
                timing='import wall and phase clocks; source setup, repository open and complete closure audit excluded; warm cache; fresh process for each import',
                memory='Linux process high-water RSS sampled immediately after import, before independent inventory/closure audit; includes repository open',
                phase_accounting='verify_sum and upload_sum overlap each other and stage_poll/drain; do not add these durations to wall time'))
        def save():
            common.write_atomic(args.output, json.dumps(result, indent=2)+'\n')
            lines = ['# Git import profile', '', f"Complete: {result['complete']}", '', args.measurement_note, '',
                     '| Shape | Operation | Objects | Wall, s | Header+decode, s | Stage poll+drain, s | Checkpoints, s | RSS at import end, MiB |',
                     '|---|---|---:|---:|---:|---:|---:|---:|']
            for s in result['samples']:
                n = s['profile']['nanos']
                rss = s.get('peak_rss_at_import_end_bytes')
                rss_text = f'{rss/1048576:.1f}' if rss is not None else 'n/a'
                lines.append(f"| {s['shape']} | {s['operation']} | {s['objects']} | {s['wall_seconds']:.3f} | {(n[2]+n[3])/1e9:.3f} | {(n[4]+n[7])/1e9:.3f} | {n[8]/1e9:.3f} | {rss_text} |")
            if 'error' in result: lines += ['', 'Error: '+result['error']]
            common.write_atomic(args.report or args.output.with_suffix('.md'), '\n'.join(lines)+'\n')
        save()
        try:
            for shape in args.shapes:
                fixture, metadata = prepare_fixture(args.fixture_root or work/'fixtures', args.profile, shape)
                result['corpora'][shape] = metadata
                save()
                for rep in range(args.repetitions):
                    destination = work/f'{shape}-{rep}'
                    initial_root = None
                    for operation, source in (('initial-import', 'base'), ('unchanged-import', 'base'), ('incremental-import', 'incremental')):
                        expected = metadata['sources'][source]
                        print(f'profiling {args.profile}/{shape} {operation}, {expected["objects"]} objects, repetition {rep}', flush=True)
                        env = {**os.environ, 'CASITA_GIT_SOURCE': str(fixture/f'{source}.git'), 'CASITA_GIT_DESTINATION': str(destination),
                               'CASITA_GIT_INVENTORY': str(fixture/f'{source}.inventory'), 'CASITA_GIT_TIP': expected['tip'], 'CASITA_GIT_OPERATION': operation}
                        command = [str(binary), PROBE, '--exact', '--ignored', '--nocapture']
                        measured = common.measured_command(common.CommandSpec([command], work, env), work/'stdout', work/'stderr', check=False)
                        stdout, stderr = (work/'stdout').read_text(), (work/'stderr').read_text()
                        result['processes'].append(dict(shape=shape, operation=operation, repetition=rep, stdout=stdout, stderr=stderr, **measured))
                        if measured['exit_code'] != 0:
                            raise common.BenchmarkError(f'profile failed: {stdout}\n{stderr}')
                        row = parse_sample(stdout, operation, expected)
                        if operation == 'initial-import':
                            initial_root = row['root']
                        elif operation == 'unchanged-import' and row['root'] != initial_root:
                            raise common.BenchmarkError('unchanged import changed its persisted root')
                        result['samples'].append(dict(status='ok', implementation='casita', shape=shape, repetition=rep, entries=row['objects'], wall_seconds=row['wall_nanos']/1e9, **row))
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
