"""Validate retained callback experiment and print median first-launch timings."""
import json
from pathlib import Path
from statistics import median
from benchmarks.suites.native_fskit_workloads import validate

report = json.loads(Path(__file__).with_name('workloads-callbacks.json').read_text())
backends = ('native-casita-repository', 'fuser-casita-repository', 'host')
counts = (1, 17, 32, 33)
assert all(report[k] for k in ('complete', 'comparison_complete', 'workloads_complete'))
assert not report['cleanup_errors']
rows = report['workload_trials']
validate(rows, 3, backends, counts)
for row in rows:
    if row['implementation'] != backends[0]:
        continue
    for phase in ('before', 'first', 'repeat'):
        c = row['counters_'+phase]
        t = c['native_read_trace']
        assert t['enabled'] and t['dropped'] == 0
        assert sum(v[0] for v in t['ranges'].values()) == c['reads']
        assert all(v[3] == 0 for v in t['ranges'].values())
        assert 0 <= t['callbacks'][0] <= c['reads'] and t['callbacks'][4] == 0
    final = row['native_teardown']
    assert final['repository_release_barrier'] == 'passed'
    assert final['stats']['native_reader_cache']['resident'] == 0
    assert final['stats']['native_read_trace']['callbacks'][0] == final['stats']['reads']
    a, b = (row['counters_'+p] for p in ('before', 'first'))
    x, y = (c['native_read_trace']['callbacks'] for c in (a, b))
    row['metrics'] = dict(zip(('calls', 'backend_ms', 'copy_ms', 'reply_ms', 'errors'),
                             [y[0]-x[0], *((y[i]-x[i])/1e6 for i in (1, 2, 3)), y[4]-x[4]]))
    row['metrics']['opens'] = b['blob_opens']-a['blob_opens']
    row['metrics']['evictions'] = b['native_reader_cache']['evictions']-a['native_reader_cache']['evictions']
    row['metrics']['directory_ms'] = (b['directory_ns']-a['directory_ns'])/1e6
    row['metrics']['range_service_ms'] = (sum(v[2] for v in b['native_read_trace']['ranges'].values())
                                          -sum(v[2] for v in a['native_read_trace']['ranges'].values()))/1e6
for pattern in ('shared', 'distinct'):
    for count in counts:
        selected = [r for r in rows if (r['pattern'], r['workers']) == (pattern, count)]
        native = [r for r in selected if r['implementation'] == backends[0]]
        output = {k: round(median(r['metrics'][k] for r in native), 3) for k in native[0]['metrics']}
        output.update({b: round(median(r['first']['wall_ns'] for r in selected if r['implementation'] == b)/1e6, 3)
                       for b in backends})
        print(pattern, count, output)
print('trials', len(rows), 'executions', sum(len(r[p]['samples']) for r in rows for p in ('first', 'repeat')))
print('load range', min(r['load'][0] for r in rows), max(r['load'][0] for r in rows))
