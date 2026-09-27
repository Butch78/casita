"""Validate the permanent matrices and print their median timings."""
import hashlib
import json
from pathlib import Path
from statistics import median

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parents[2]


def read(path):
    return json.loads(path.read_text())


def replay(name, repetitions):
    from benchmarks.suites.decoded_seek_replay import CASES, parse_sample
    r = read(ROOT / name)
    assert r['complete'] and not r.get('error')
    expected = {(rep, case, cap) for rep in range(repetitions) for case in CASES for cap in (0, 2097152)}
    assert len(r['samples']) == len(expected)
    assert {(s['repetition'], s['case'], s['capacity']) for s in r['samples']} == expected
    for row in r['samples']:
        parse_sample('seek_replay_sample ' + json.dumps(row) + '\n1 passed;', row['case'], row['capacity'], row['cycles'])
        assert row['chunk_range_requests'] == 0
    print(name)
    for case in CASES:
        for cap in (0, 2097152):
            selected = [s for s in r['samples'] if (s['case'], s['capacity']) == (case, cap)]
            print(case, cap, {k: round(median(s[k] for s in selected) / (1e6 if k.endswith('_ns') else 1), 4)
                for k in ('elapsed_ns', 'decode_calls', 'decoded_bytes', 'fetch_ns', 'decode_ns', 'cache_bytes', 'shared_cache_bytes')})
    assert all(hashlib.sha256((REPO / p).read_bytes()).hexdigest() == digest for p, digest in r['source_sha256'].items())
    return r


def mounted():
    paths = [ROOT.parent / '2026-09-18-native-fskit-read-ranges/workloads-read-control.json',
             ROOT.parent / '2026-09-18-decoded-seek-replay/workloads-decoded-cache-retry.json',
             ROOT / 'workloads-stable-cache.json']
    reports = [read(path) for path in paths]
    expected = {(rep, pattern, workers, backend) for rep in range(3) for pattern in ('shared', 'distinct')
                for workers in (1, 8, 15, 16, 17) for backend in ('native-casita-repository', 'fuser-casita-repository', 'host')}
    for r in reports:
        assert r['complete'] and r['comparison_complete'] and r['workloads_complete'] and not r['cleanup_errors']
        assert len(r['workload_trials']) == len(expected)
        assert {(s['repetition'], s['pattern'], s['workers'], s['implementation']) for s in r['workload_trials']} == expected
        assert r['workload_fixture'] == reports[0]['workload_fixture'] and r['snapshot'] == reports[0]['snapshot']
        for row in r['workload_trials']:
            assert row['correctness'] == row['teardown'] == 'passed'
            for phase in ('first', 'repeat'):
                assert len(row[phase]['samples']) == row['workers']
                assert all(s['correctness'] == 'passed' for s in row[phase]['samples'])
            if row['implementation'] == 'native-casita-repository':
                assert row['native_teardown']['stats']['native_reader_cache']['resident'] == 0
    for pattern in ('shared', 'distinct'):
        for workers in (1, 8, 15, 16, 17):
            for backend in ('native-casita-repository', 'fuser-casita-repository', 'host'):
                rows = [[s for s in r['workload_trials'] if (s['pattern'], s['workers'], s['implementation']) == (pattern, workers, backend)] for r in reports]
                print(pattern, workers, backend, {phase: [round(median(s[phase]['wall_ns'] for s in group) / 1e6, 3) for group in rows] for phase in ('first', 'repeat')})
    assert all(hashlib.sha256((REPO / p).read_bytes()).hexdigest() == digest for p, digest in reports[-1]['source_sha256'].items())


if __name__ == '__main__':
    real = replay('stable-cache-awk.json', 3)
    synthetic = replay('stable-cache-synthetic.json', 1)
    assert real['artifacts'] == synthetic['artifacts'] and real['source_sha256'] == synthetic['source_sha256']
    if (ROOT / 'workloads-stable-cache.json').exists():
        mounted()
