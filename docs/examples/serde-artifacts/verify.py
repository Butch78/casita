#!/usr/bin/env python3
"""Build Serde itself and record its output files for the docs illustration."""
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import urllib.request

version = '1.0.229'
checksum = '4148590afebada386688f18773da617792bf2ef03ffc1e4cbd2b1d45b023e0ba'
crate = f'serde-{version}.crate'
cache = Path(os.environ.get('CARGO_HOME', str(Path.home() / '.cargo'))) / 'registry/cache'
cached = next(cache.glob(f'*/{crate}'), None)
data = cached.read_bytes() if cached else urllib.request.urlopen(f'https://static.crates.io/crates/serde/{crate}').read()
assert hashlib.sha256(data).hexdigest() == checksum, 'Serde source checksum mismatch'
report = {
    'project': 'serde', 'version': version,
    'source': f'https://static.crates.io/crates/serde/{crate}',
    'source_sha256': checksum,
    'rustc': subprocess.check_output(['rustc', '--version'], text=True).strip(),
    'command': 'cargo build --lib --locked --message-format=json',
    'files': [],
    'illustration': 'Output filenames are real. Chunk boundaries, counts, sharing and transfer are schematic, not measured.',
}
with tempfile.TemporaryDirectory(prefix='casita-build-serde-') as temp:
    directory = Path(temp)
    with tarfile.open(fileobj=io.BytesIO(data), mode='r:gz') as archive:
        archive.extractall(directory, filter='data')
    project = directory / f'serde-{version}'
    env = dict(os.environ, CARGO_TARGET_DIR=str(directory / 'target'))
    subprocess.run(['cargo', 'generate-lockfile'], cwd=project, env=env, check=True)
    result = subprocess.run(['cargo', 'build', '--lib', '--locked', '--message-format=json'], cwd=project, env=env, capture_output=True, text=True, check=True)
    files = []
    for line in result.stdout.splitlines():
        item = json.loads(line)
        if item.get('reason') == 'compiler-artifact' and item['target']['name'] == 'serde':
            files = [Path(f) for f in item['filenames'] if f.endswith(('.rlib', '.rmeta'))]
    assert {f.suffix for f in files} == {'.rlib', '.rmeta'}, files
    rlib = next(f for f in files if f.suffix == '.rlib')
    assert rlib == directory / 'target/debug/libserde.rlib'
    assert rlib.is_file()
    report['primary_artifact'] = 'target/debug/libserde.rlib'
    report['profile'] = 'dev'
    depfiles = [f for f in (directory / 'target').rglob('*.d') if 'serde' in f.name and 'serde_core' not in f.name]
    assert depfiles, [str(f.relative_to(directory)) for f in (directory / 'target/debug').rglob('*') if f.is_file()]
    depinfo = depfiles[0]
    assert depinfo.is_file(), depinfo
    files.append(depinfo)
    labels = {'.rlib': ('libserde.rlib', 'Compiled library'), '.rmeta': ('libserde.rmeta', 'Compiler metadata'), '.d': ('libserde.d', 'Dependency file')}
    for f in files:
        label, kind = labels[f.suffix]
        content = f.read_bytes()
        report['files'].append({'label': label, 'kind': kind, 'path': f.name, 'bytes': len(content), 'sha256': hashlib.sha256(content).hexdigest()})
    second = subprocess.run(['cargo', 'build', '--lib', '--locked', '--message-format=json'], cwd=project, env=env, capture_output=True, text=True, check=True)
    artifacts = [item for line in second.stdout.splitlines() if (item := json.loads(line)).get('reason') == 'compiler-artifact']
    serde = next(item for item in artifacts if item['target']['name'] == 'serde')
    assert serde['fresh'], 'The unchanged Serde build unexpectedly recompiled'
    unchanged = all(hashlib.sha256(f.read_bytes()).hexdigest() == item['sha256'] for f, item in zip(files, report['files']))
    assert unchanged, 'The second build changed an output file'
    recompiled = sum(not item['fresh'] for item in artifacts)
    assert recompiled == 0, artifacts
    report['second_build'] = {'same_workspace_and_inputs': True, 'serde_fresh': serde['fresh'], 'recompiled_targets': recompiled, 'output_bytes_unchanged': unchanged}
    report['verified'] = {'source_checksum': True, 'built_serde_itself': True, 'all_output_files_exist': True}
output = Path(__file__).resolve().parents[2] / 'public/examples/serde-artifacts.json'
output.write_text(json.dumps(report, indent=2) + '\n')
print(f'Built Serde {version}: ' + ', '.join(f['label'] for f in report['files']))
print(output)
