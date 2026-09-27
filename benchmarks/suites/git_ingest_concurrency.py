"""Bounded native Git ingestion, with initial/incremental identity and checkout gates."""
from __future__ import annotations

import argparse
import base64
import hashlib
import itertools
import json
import pathlib
import random
import shutil
import subprocess
import tempfile

from benchmarks.suites import repository as common
from benchmarks.suites.git import git_env, write_fast_import_data
from benchmarks.suites.lifecycle import root_keys
from benchmarks.suites.metadata_collection import positive_csv


def layouts_csv(value):
    names = value.split(',')
    if len(names) != len(set(names)) or any(n not in ('loose', 'packed', 'delta') for n in names):
        raise argparse.ArgumentTypeError('expected distinct loose,packed,delta names')
    return names


def key(kind, oid):
    return f'git.sha1.{kind}.v1:' + base64.urlsafe_b64encode(bytes.fromhex(oid)).decode().rstrip('=')


def fixture(work, files, layout):
    """Two deterministic revisions, including unchanged objects in the second tree."""
    import io
    sources = []
    previous = None
    for version in range(2):
        source = work / f'{layout}-{files}-{version}.git'
        if previous is None:
            common.run_checked(['git', 'init', '-q', '--bare', '-b', 'main', '--object-format=sha1', str(source)], env=git_env())
        else:
            shutil.copytree(previous, source)
        stream = io.BytesIO()
        changed = range(files) if version == 0 else range(0, files, 4)
        for index in changed:
            size = 65536 if index % 16 == 0 or layout == 'delta' else 1024
            data = bytearray(random.Random(index).randbytes(size))
            data[:16] = f'{index:08}-{version:07}'.encode()
            stream.write(f'blob\nmark :{index + 1}\n'.encode())
            write_fast_import_data(stream, data)
        stream.write(f'commit refs/heads/main\ncommitter Benchmark <benchmark@invalid> {1700000000 + version} +0000\n'.encode())
        write_fast_import_data(stream, f'commit {version}'.encode())
        if previous:
            parent = common.run_checked(['git', f'--git-dir={previous}', 'rev-parse', 'HEAD'], env=git_env()).strip()
            stream.write(f'from {parent}\n'.encode())
        for index in changed:
            stream.write(f'M 100644 :{index + 1} files/{index:06}\n'.encode())
        stream.write(b'\ndone\n')
        subprocess.run(['git', f'--git-dir={source}', 'fast-import', '--quiet'], input=stream.getvalue(), env=git_env(), check=True, capture_output=True)
        if layout == 'loose':
            loose = work / f'unpacked-{files}-{version}'
            loose.mkdir()
            env = {**git_env(), 'GIT_OBJECT_DIRECTORY': str(loose.resolve())}
            for pack in (source / 'objects/pack').glob('*.pack'):
                with pack.open('rb') as handle:
                    subprocess.run(['git', f'--git-dir={source}', 'unpack-objects', '-r'], stdin=handle, env=env, check=True, capture_output=True)
            for directory in loose.iterdir():
                shutil.copytree(directory, source / 'objects' / directory.name, dirs_exist_ok=True)
            shutil.rmtree(source / 'objects/pack')
        else:
            common.run_checked(['git', f'--git-dir={source}', 'repack', '-adf', '--window=16' if layout == 'delta' else '--window=0'], env=git_env())
        common.run_checked(['git', f'--git-dir={source}', 'fsck', '--full', '--strict'], env=git_env())
        checkout = work / f'expected-{layout}-{files}-{version}'
        checkout.mkdir()
        common.run_checked(['git', f'--git-dir={source}', f'--work-tree={checkout}', 'read-tree', 'HEAD'], env=git_env())
        common.run_checked(['git', f'--git-dir={source}', f'--work-tree={checkout}', 'checkout-index', '-a'], env=git_env())
        tree = common.run_checked(['git', f'--git-dir={source}', 'rev-parse', 'HEAD^{tree}'], env=git_env()).strip()
        tip = common.run_checked(['git', f'--git-dir={source}', 'rev-parse', 'HEAD'], env=git_env()).strip()
        count = int(common.run_checked(['git', f'--git-dir={source}', 'rev-list', '--objects', '--all', '--count'], env=git_env()))
        sources.append((source, common.tree_manifest(checkout), key('tree', tree), key('commit', tip), count))
        previous = source
    return sources


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--profile', choices=('smoke', 'standard'), default='standard')
    parser.add_argument('--counts', type=positive_csv)
    parser.add_argument('--layouts', type=layouts_csv, default=['loose', 'packed', 'delta'])
    parser.add_argument('--concurrency', type=positive_csv, default=[1, 16])
    parser.add_argument('--max-buffered-bytes', type=positive_csv, default=[65535, 65536, 65537, 67108864])
    parser.add_argument('--omit-limits', action='store_true', help='Measure an older serial CLI without the new flags; requires concurrency 1 and one budget.')
    parser.add_argument('--repetitions', type=int, default=3)
    parser.add_argument('--casita-bin', type=pathlib.Path, default=pathlib.Path('target/release/casita'))
    parser.add_argument('--no-build', action='store_true')
    parser.add_argument('--output', type=pathlib.Path, required=True)
    parser.add_argument('--report', type=pathlib.Path)
    parser.add_argument('--measurement-note', default='')
    args = parser.parse_args(argv)
    if args.repetitions < 1 or (args.omit_limits and (args.concurrency != [1] or len(args.max_buffered_bytes) != 1)):
        parser.error('positive repetitions required; --omit-limits requires concurrency 1 and one budget')
    if not args.no_build:
        common.run_checked(['cargo', 'build', '--release', '--features', 'cli,git', '--bin', 'casita'])
    binary = args.casita_bin.resolve()
    adapter = common.CasitaAdapter(str(binary))
    counts = args.counts or ([17] if args.profile == 'smoke' else [15, 16, 17, 256])
    with binary.open('rb') as handle:
        digest = hashlib.file_digest(handle, 'sha256').hexdigest()
    with tempfile.TemporaryDirectory(prefix='casita-git-ingest-') as temporary:
        work = pathlib.Path(temporary)
        result = dict(schema_version=1, result_schema='casita.git-ingest-concurrency.v1', suite_id='native-git', complete=False,
                      environment=common.environment_metadata(work), artifacts=[dict(path=str(binary), sha256=digest)], samples=[],
                      configuration=dict(counts=counts, layouts=args.layouts, concurrency=args.concurrency, max_buffered_bytes=args.max_buffered_bytes,
                                         omit_limits=args.omit_limits, repetitions=args.repetitions, measurement_note=args.measurement_note,
                                         timing='CLI initial/incremental durable imports; setup and correctness excluded; warm source cache; native pack cache disabled'))
        def save():
            common.write_atomic(args.output, json.dumps(result, indent=2) + '\n')
            if args.report:
                lines = ['# Git ingestion concurrency', '', f"Complete: {result['complete']}", '', args.measurement_note, '',
                         '| Layout | Files | Concurrency | Byte budget | Operation | Repetition | Seconds |', '|---|---:|---:|---:|---|---:|---:|']
                for row in result['samples']:
                    lines.append(f"| {row['layout']} | {row['entries']} | {row['concurrency']} | {row['max_buffered_bytes']} | {row['operation']} | {row['repetition']} | {row['wall_seconds']:.6f} |")
                common.write_atomic(args.report, '\n'.join(lines) + '\n')
        save()
        expected_roots = {}
        try:
            sources = {(files, layout): fixture(work, files, layout) for files in counts for layout in args.layouts}
            jobs = list(itertools.product(counts, args.layouts, args.concurrency, args.max_buffered_bytes, range(args.repetitions)))
            random.Random(1729).shuffle(jobs)
            for index, (files, layout, concurrency, budget, repetition) in enumerate(jobs):
                repository = work / f'repository-{index}'
                adapter.init(repository)
                for version, (source, manifest, tree, tip, count) in enumerate(sources[files, layout]):
                    operation = 'initial-import' if version == 0 else 'incremental-import'
                    print(f'git-ingest: {layout}, files={files}, concurrency={concurrency}, bytes={budget}, {operation}, repetition={repetition}', flush=True)
                    command = adapter.command(repository, 'import', str(source), '-i', 'git', '--git-view', 'bench', '--git-max-cached-pack-bytes', '0')
                    if not args.omit_limits:
                        command += ['--git-concurrency', str(concurrency), '--git-max-buffered-bytes', str(budget)]
                    timing = common.measured_command(common.CommandSpec([command], work, adapter.env()), work/'stdout', work/'stderr')
                    stdout = (work/'stdout').read_text()
                    root = root_keys(adapter, repository).get('git/bench')
                    if root is None or root != expected_roots.setdefault((files, layout, version), root) or f'objects {count}\n' not in stdout:
                        raise common.BenchmarkError('wrong Git object count or changed view identity')
                    shown = common.run_checked(adapter.command(repository, 'git', 'show', 'bench'), env=adapter.env())
                    if f'view {root}\n' not in shown or f'ref refs/heads/main {tip}\n' not in shown:
                        raise common.BenchmarkError('wrong reopened Git view or ref')
                    checkout = work / 'checkout'
                    common.run_checked(adapter.command(repository, 'git', 'checkout', tree, str(checkout)), env=adapter.env())
                    common.assert_manifest(checkout, manifest)
                    common.run_checked(adapter.fsck_command(repository), env=adapter.env())
                    shutil.rmtree(checkout)
                    result['samples'].append(dict(status='ok', implementation='casita', operation=operation, entries=files, layout=layout,
                        concurrency=concurrency, max_buffered_bytes=budget, repetition=repetition, root=root, objects=count,
                        correctness='source object count, reopened view/ref identity, exact checkout manifest, fsck', command=command, **timing))
                    save()
                shutil.rmtree(repository)
            result['complete'] = True
        except Exception as error:
            result['error'] = str(error)
            raise
        finally:
            save()
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
