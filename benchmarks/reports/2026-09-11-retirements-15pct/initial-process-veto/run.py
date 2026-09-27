"""Paired runs of the permanent online-holds cases; no scheduling sweep."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[2]
sys.path.insert(0, str(REPO))
from benchmarks.host_activity import QuietHost
from benchmarks.suites.repository import BenchmarkError

BINARIES = {
    'before': ROOT / 'bin/before',
    'after': ROOT / 'bin/after',
}
CPUS = '8,10,12,14'
hashes = {name: hashlib.sha256(path.read_bytes()).hexdigest()
          for name, path in BINARIES.items()}

(ROOT / 'protocol.json').write_text(json.dumps({
    'before_commit': '450a633', 'after_commit': 'b279897',
    'lockfile_sha256': hashlib.sha256((ROOT / 'Cargo.lock').read_bytes()).hexdigest(),
    'binaries': {name: str(path) for name, path in BINARIES.items()},
    'sha256': hashes, 'cpus': CPUS, 'imports': [60, 300],
    'pairs': 3, 'warmups': 'one per variant and size',
    'files_per_import': 16, 'reader_scope': 'application', 'gc_delay_ms': 5,
    'baseline_harness': 'identical tracked benchmark source and copied Cargo.lock; both builds use --locked',
    'host': 'shared; paired timing accepted only when BOTH runs pass all quiet-host samples',
    'quiet_host': {'timeout': 90, 'quiet_seconds': 5, 'max_cpu_fraction': .15},
    'max_pair_attempts_per_size': 6, 'target_clean_pairs_per_size': 3,
}, indent=2))

def run(imports, pair, variant):
    name = f'{imports}-{pair}-{variant}'
    env = dict(os.environ, CASITA_BENCH_IMPORTS=str(imports), CASITA_BENCH_FILES='16',
               CASITA_BENCH_SCENARIO='imports_readers_gc',
               CASITA_BENCH_READER_SCOPE='application', CASITA_BENCH_GC_DELAY_MS='5')
    command = ['taskset', '-c', CPUS, str(BINARIES[variant])]
    guard = QuietHost(timeout=90, quiet_seconds=5, max_cpu_fraction=.15)
    print(json.dumps(dict(event='waiting_for_quiet_host', imports=imports, pair=pair, variant=variant)), flush=True)
    try:
        with guard:
            with (ROOT / f'{name}.log').open('w') as output:
                try:
                    code = subprocess.run(command, env=env, stdout=output,
                                          stderr=subprocess.STDOUT, timeout=350).returncode
                except subprocess.TimeoutExpired:
                    code = 124
    except BenchmarkError as error:
        (ROOT / 'blocked.json').write_text(json.dumps(dict(imports=imports, pair=pair,
            variant=variant, reason=str(error), last_sample=guard.sample()), indent=2))
        raise
    host = guard.report()
    rows = [json.loads(line) for line in (ROOT / f'{name}.log').read_text().splitlines()
            if line.startswith('{')]
    errors = []
    try:
        assert code == 0 and len(rows) == 1
        row = rows[0]
        assert row['imports'] == imports and row['files_per_import'] == 16
        assert row['reader_scope'] == 'application'
        assert row['gc_passes'] + row['gc_busy'] + row['gc_retryable_errors'] == len(row['gc_attempts'])
        assert sum(row['gc_busy_reasons'].values()) == row['gc_busy']
        assert sum(row['gc_retryable_error_reasons'].values()) == row['gc_retryable_errors']
        passes = row['gc_completed_passes']
        assert len(passes) == row['gc_passes']
        assert sum(p['removed_objects'] for p in passes) == row['gc_removed_objects']
        active = [p for p in passes if p['elapsed_seconds'] <= row['import_seconds']]
        assert len(active) == row['gc_passes_during_imports']
        assert sum(p['removed_objects'] for p in active) == row['gc_removed_objects_during_imports']
        assert row['gc_removed_objects'] + row['cleanup_removed_objects'] == (imports - 1) * 17
        for attempt in row['gc_attempts']:
            assert attempt['finished_seconds'] >= attempt['started_seconds']
            for phase in attempt['phases']:
                assert phase['seconds'] >= 0
                assert phase['finished_seconds'] <= attempt['finished_seconds'] + .02
                assert phase['finished_seconds'] - phase['seconds'] >= attempt['started_seconds'] - .02
    except AssertionError:
        errors.append('exit, sample, or accounting validation failed')
    record = dict(imports=imports, pair=pair, variant=variant, warmup=pair == 0,
                  complete=not errors, exit_code=code, errors=errors,
                  command=command, samples=rows, host_activity=host, quiet=host['quiet'])
    (ROOT / f'{name}.json').write_text(json.dumps(record, indent=2))
    print(json.dumps({k: record[k] for k in ['imports', 'pair', 'variant', 'complete', 'quiet']}), flush=True)
    if errors:
        raise RuntimeError(name + ': ' + str(errors))
    return record

pairs = []
for imports in [60, 300]:
    for variant in ['before', 'after']:
        run(imports, 0, variant)
    accepted = 0
    for attempt in range(1, 7):
        rows = [run(imports, attempt, variant) for variant in
                (['before', 'after'] if attempt % 2 else ['after', 'before'])]
        clean = all(row['quiet'] for row in rows)
        pairs.append(dict(imports=imports, attempt=attempt, accepted=clean))
        (ROOT / 'pairs.json').write_text(json.dumps(pairs, indent=2))
        print(json.dumps(dict(event='pair_finished', imports=imports, attempt=attempt, accepted=clean)), flush=True)
        accepted += clean
        if accepted == 3:
            break
    if accepted != 3:
        raise RuntimeError(f'Only {accepted} uncontaminated pairs at {imports} imports; retained all attempts')
for name, path in BINARIES.items():
    assert hashlib.sha256(path.read_bytes()).hexdigest() == hashes[name]
