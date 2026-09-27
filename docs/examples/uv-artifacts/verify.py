#!/usr/bin/env python3
"""Build pinned uv sources and verify the executable shown in the docs."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import urllib.request

VERSION = '0.12.7'
COMMIT = '61291a8ca5477a9ca653f14d2ac5665587c263fa'
CHECKSUM = '082a770752bf1f58e2bc46cd797817a15b1f494b3aa3f7fc313d209125b7151e'
SOURCE = f'https://codeload.github.com/astral-sh/uv/tar.gz/{COMMIT}'
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--source-archive', type=Path)
parser.add_argument('--work-dir', type=Path, help='Keep build files here to allow incremental reruns')
args = parser.parse_args()


def sha256(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def verify(directory):
    directory.mkdir(parents=True, exist_ok=True)
    archive = args.source_archive or directory / 'source.tar.gz'
    if not archive.exists():
        urllib.request.urlretrieve(SOURCE, archive)
    assert sha256(archive) == CHECKSUM, 'uv source archive checksum mismatch'
    with tarfile.open(archive, 'r:gz') as source:
        source.extractall(directory, filter='data')
    project = directory / f'uv-{COMMIT}'
    target = directory / 'target'
    env = dict(os.environ, CARGO_TARGET_DIR=str(target), CARGO_BUILD_JOBS=os.environ.get('CARGO_BUILD_JOBS', '4'))
    command = ['cargo', 'build', '--locked', '-p', 'uv', '--bin', 'uv', '--message-format=json']
    log = directory / 'cargo-artifacts.jsonl'
    with log.open('w') as output:
        subprocess.run(command, cwd=project, env=env, stdout=output, check=True)
    artifacts = [item for line in log.read_text().splitlines() if (item := json.loads(line)).get('reason') == 'compiler-artifact']
    binary = next(item for item in artifacts if item['target']['name'] == 'uv' and item.get('executable'))
    executable = Path(binary['executable'])
    assert executable == target / 'debug/uv', executable
    assert executable.is_file() and os.access(executable, os.X_OK)
    version_output = subprocess.check_output([str(executable), '--version'], text=True).strip()
    assert version_output.startswith(f'uv {VERSION}'), version_output
    library = next(Path(filename) for item in artifacts if item['target']['name'] == 'uv' and 'lib' in item['target']['kind'] for filename in item['filenames'] if filename.endswith('.rlib'))
    depinfo = target / 'debug/uv.d'
    assert library.is_file() and depinfo.is_file()
    files = [(executable, 'uv', 'Executable'), (library, 'libuv.rlib', 'Supporting library'), (depinfo, 'uv.d', 'Dependency information')]
    report = {
        'project': 'uv', 'version': VERSION, 'source': SOURCE, 'source_commit': COMMIT,
        'source_sha256': CHECKSUM, 'rustc': subprocess.check_output(['rustc', '--version'], text=True).strip(),
        'command': ' '.join(command), 'profile': 'dev', 'primary_artifact': 'target/debug/uv',
        'files': [{'label': label, 'kind': kind, 'path': path.name, 'bytes': path.stat().st_size, 'sha256': sha256(path)} for path, label, kind in files],
        'run': {'command': './target/debug/uv --version', 'stdout': version_output, 'exit_code': 0},
        'verified': {'source_checksum': True, 'built_uv_itself': True, 'all_output_files_exist': True, 'executable_runs': True},
        'illustration': 'Real build outputs; supporting-library labels omit Cargo hash suffixes. Chunk boundaries, counts and sharing are schematic. This ordinary Cargo build does not implement or validate the proposed Casita integration.',
    }
    output = Path(__file__).resolve().parents[2] / 'public/examples/uv-artifacts.json'
    output.write_text(json.dumps(report, indent=2) + '\n')
    print(version_output)
    print(output)


if args.work_dir:
    verify(args.work_dir.resolve())
else:
    with tempfile.TemporaryDirectory(prefix='casita-build-uv-') as temp:
        verify(Path(temp))
