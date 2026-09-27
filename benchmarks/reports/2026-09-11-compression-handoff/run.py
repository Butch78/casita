import hashlib
import itertools
import json
import os
import pathlib
import platform
import statistics
import subprocess
import sys
import threading

ROOT = pathlib.Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))
os.chdir(ROOT)
from benchmarks.host_activity import QuietHost

binary = pathlib.Path(sys.argv[1]).resolve()
output = pathlib.Path(sys.argv[2]).resolve()
output.mkdir(parents=True, exist_ok=False)
def sha(path):
    with path.open('rb') as handle:
        return hashlib.file_digest(handle, 'sha256').hexdigest()

fingerprint = sha(binary)
monitor = QuietHost()
activity = []
def sample():
    while not monitor.stop.is_set():
        interval = monitor.sample()
        if interval['interval_seconds'] >= 0.5:
            activity.append(interval)

thread = threading.Thread(target=sample, daemon=True)
command = [str(binary), '--test']
thread.start()
try:
    with (output / 'run.log').open('w') as log:
        process = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, timeout=600)
finally:
    monitor.stop.set()
    thread.join()
captured = (output / 'run.log').read_text()
rows = [json.loads(line) for line in captured.splitlines() if line.startswith('{')]
def identity(row):
    return tuple(row[key] for key in ('workers', 'size', 'flavor', 'concurrency', 'repetition', 'variant'))

variants = tuple(sys.argv[3:]) or ('blocking', 'inline', 'inline_4k_yield')
expected = set(itertools.product((1,4), (1024,4095,4096,4097,16384,65536),
                                ('random','text'), (1,16), range(4), variants))
valid = (process.returncode == 0 and len(rows) == len(expected)
         and {identity(row) for row in rows} == expected
         and captured.count('\nSuccess') == 48 * len(variants)
         and all(row['correctness'] == 'frame length and complete roundtrip for every indexed chunk'
                 and row['chunks'] == 64 and row['ready_task_polls'] > 0 for row in rows)
         and sha(binary) == fingerprint)
report = {'schema': 'casita.compression-handoff-study.v1', 'complete': valid,
          'command': command, 'binary_sha256': fingerprint, 'exit_code': process.returncode,
          'environment': {'platform': platform.platform(), 'cpu_count': os.cpu_count(),
                          'rustc': subprocess.check_output(['rustc', '-Vv'], text=True)},
          'source_sha256': {name: sha(pathlib.Path(name)) for name in
                            ('benches/compression_handoff.rs', 'src/compression.rs')},
          'host_activity': activity, 'samples': rows,
          'timing_accepted': False,
          'scope': 'Exploratory codec scheduling under recorded host load. No quiet-host admission; no end-to-end throughput claim.'}
if valid:
    indexed = {identity(row): row for row in rows}
    comparisons = []
    for key in itertools.product((1,4), (1024,4095,4096,4097,16384,65536), ('random','text'), (1,16)):
        for variant in variants[1:]:
            result = dict(zip(('workers','size','flavor','concurrency'), key), variant=variant)
            for metric in ('wall_ns','longest_poll_ns','ready_task_gap_ns'):
                before = [indexed[(*key, repetition, 'blocking')][metric] for repetition in range(4)]
                after = [indexed[(*key, repetition, variant)][metric] for repetition in range(4)]
                result[metric] = {'blocking_median': statistics.median(before),
                                  'candidate_median': statistics.median(after),
                                  'paired_median_ratio': statistics.median(a / b for a,b in zip(after,before))}
            comparisons.append(result)
    report['comparisons'] = comparisons
(output / 'report.json').write_text(json.dumps(report, indent=2)+'\n')
print('complete:', valid, 'samples:', len(rows), 'host intervals:', len(activity), flush=True)
if valid:
    for row in report['comparisons']:
        if row['size'] in (1024,4096,65536):
            print(row, flush=True)
raise SystemExit(0 if valid else 1)
