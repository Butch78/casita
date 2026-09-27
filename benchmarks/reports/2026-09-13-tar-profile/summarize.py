"""Retain scoped tar profiles and derive non-overlapping symbol categories.

Usage: python3 summarize.py RAW_PROFILE_DIRECTORY OUTPUT_JSON BUILD_PROVENANCE_JSON
"""
import hashlib
import json
import pathlib
import re
import statistics
import sys


def sha(path):
    with path.open('rb') as handle:
        return hashlib.file_digest(handle, 'sha256').hexdigest()


def main(raw, output, provenance):
    report = json.loads((raw / 'report.json').read_text())
    build = json.loads(provenance.read_text())
    if sha(pathlib.Path(report['binary'])) != build['binary_sha256'] or report['binary_sha256'] != build['binary_sha256']:
        raise ValueError('binary does not match build provenance')
    report['build_provenance'] = build
    if not report['complete'] or len(report['runs']) != 6:
        raise ValueError('expected all six completed permanent profile cases')
    for row in report['runs']:
        shape, concurrency = row['case'].split('/')[-2:]
        flat = raw / f'{shape}-{concurrency}.perf-flat.txt'
        log = raw / f'{shape}-{concurrency}.log'
        data = raw / f'{shape}-{concurrency}.perf.data'
        if sha(data) != row['data_sha256']:
            raise ValueError('profile recording changed')
        text = flat.read_text()
        captured = log.read_text()
        samples = re.search(r'Captured and wrote .*\((\d+) samples\)', captured)
        lost = re.search(r'Total Lost Samples:\s*(\d+)', text)
        if not samples or int(samples[1]) < 100 or not lost or int(lost[1]):
            raise ValueError('undersampled or lossy profile')
        memory = sum(float(percent) for percent, symbol in re.findall(
            r'^\s+(\d+\.\d+)%\s+\S+\s+\S+\s+\[\.\]\s+(.*)$', text, re.MULTILINE)
            if any(name in symbol.lower() for name in ('memcpy', 'memmove', 'memset', 'memcmp')))
        groups = dict(row['self_percent_by_symbol_category'])
        groups['memory_operations'] = memory
        groups['other'] -= memory  # These libc symbols were previously classified as other.
        row['derived_self_percent'] = groups
        row.update(recorded_samples=int(samples[1]), lost_samples=int(lost[1]),
                   flat_report_text=text, flat_report_sha256=sha(flat), run_log=captured)
        activity = row['host_activity']
        row['background_cpu'] = {
            'median_percent': statistics.median(x['external_cpu_fraction'] for x in activity) * 100,
            'peak_percent': max(x['external_cpu_fraction'] for x in activity) * 100,
            'intervals_above_30_percent': sum(x['external_cpu_fraction'] > 0.3 for x in activity),
            'intervals': len(activity),
        }
    callers = raw / 'large-16.memmove-callers.txt'
    report['memmove_callers'] = {'case': 'tar_import_pipeline/large/16',
        'text': callers.read_text(), 'sha256': sha(callers),
        'command': [report['perf'], 'report', '--stdio', '--no-children', '--call-graph',
                    'graph,0.5,caller', '--symbol-filter', '__memmove_avx512_unaligned_erms',
                    '-i', str(raw / 'large-16.perf.data')],
        'interpretation': 'Use only resolved local caller edges; unresolved addresses prevent full call-chain attribution.'}
    report['timing_accepted'] = False
    report['profile_policy'] = 'Diagnostic CPU attribution; no quiet-host admission and no wall-clock speedup claim. Background load is retained, including intervals above the user timing ceiling.'
    report['postprocessor_sha256'] = sha(pathlib.Path(__file__))
    output.write_text(json.dumps(report, indent=2) + '\n')
    for row in report['runs']:
        groups = row['derived_self_percent']
        print(row['case'], row['recorded_samples'],
              {key: round(groups[key], 2) for key in ('blake3', 'memory_operations', 'zstd', 'fastcdc')})


if __name__ == '__main__':
    main(pathlib.Path(sys.argv[1]).resolve(), pathlib.Path(sys.argv[2]).resolve(),
         pathlib.Path(sys.argv[3]).resolve())
