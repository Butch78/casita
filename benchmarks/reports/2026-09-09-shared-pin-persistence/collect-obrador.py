"""Retain graph-runner evidence without copying private stores or build outputs."""
import argparse
import gzip
import hashlib
import importlib.util
import json
import pathlib

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('source', type=pathlib.Path)
parser.add_argument('destination', type=pathlib.Path)
args = parser.parse_args()
args.destination.mkdir(parents=True, exist_ok=True)
spec = importlib.util.spec_from_file_location('analyzer', pathlib.Path(__file__).with_name('analyze-trace.py'))
analyzer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(analyzer)
execution = json.loads((args.source/'execution.json').read_text())
files = []

def retain(source, relative):
    destination = args.destination/relative
    destination.parent.mkdir(parents=True, exist_ok=True)
    content = source.read_bytes()
    if source.name.endswith('.trace.jsonl'):
        destination = destination.with_suffix(destination.suffix+'.gz')
        destination.write_bytes(gzip.compress(content, mtime=0))
    else:
        destination.write_bytes(content)
    files.append({'path':str(destination.relative_to(args.destination)), 'source_bytes':len(content),
                  'source_sha256':hashlib.sha256(content).hexdigest(),
                  'retained_sha256':hashlib.sha256(destination.read_bytes()).hexdigest()})

for name in ['execution.json','run.py','resume.py','baseline.Cargo.toml','baseline.Cargo.lock','after.Cargo.toml','after.Cargo.lock']:
    source = args.source/name
    if source.is_file(): retain(source, pathlib.Path(name))
for run in execution['runs']:
    label = run['label']
    log = args.source/f'{label}.log'
    if log.exists(): retain(log, pathlib.Path(log.name))
    directory = args.source/label
    for name in ['results.json','summary.md']:
        source = directory/name
        if source.exists(): retain(source, pathlib.Path(label)/name)
    result = directory/'results.json'
    if result.exists() and run.get('exit_code') == 0:
        result = json.loads(result.read_text())
        workloads = execution.get('workloads', ['chain', 'wide'])
        expected = len(workloads) * (1 if run['traced'] else 2)
        assert len(result['trials']) == expected, label
        for trial in result['trials']:
            assert trial['workload'] in workloads, (label, trial)
            assert trial['verified_builds'] == 64, (label,trial)
            assert trial['verified_outputs'] == (1 if trial['workload']=='chain' else 32), (label,trial)
    for trial in directory.glob('*-round-*-jobs-8'):
        for parent in [trial, trial/'nix', trial/'obrador']:
            if not parent.is_dir(): continue
            for source in parent.iterdir():
                if source.is_file(): retain(source, source.relative_to(args.source))
        trace = trial/'obrador/build.trace.jsonl'
        if run['traced'] and trace.exists():
            summary = analyzer.summarize(trace, diagnostics=True)
            destination = args.destination/label/f'{trial.name}-diagnostics.json'
            destination.write_text(json.dumps(summary,indent=2)+'\n')
(args.destination/'files.json').write_text(json.dumps(files,indent=2)+'\n')
print(f'Retained {len(files)} files; verified every successful trial.')
