"""Summarize successful native callback ranges, separate from batch wall time."""
import json
import hashlib
from pathlib import Path
from statistics import median
import sys

ROOT = Path(__file__).resolve().parent


def ranges(row, phase):
    before = row['counters_before']['native_read_trace']['ranges']
    after = row['counters_'+phase]['native_read_trace']
    assert after['enabled'] and after['dropped'] == 0
    result = {}
    for key, values in after['ranges'].items():
        inode, offset, length = map(int, key.split(':'))
        name = after['files'][str(inode)]
        delta = [a-b for a,b in zip(values, before.get(key,[0]*4))]
        assert delta[3] == 0
        if delta[0]:
            result[(name,offset,length)] = delta
    return result


if __name__ == '__main__':
    report = json.loads(Path(sys.argv[1] if len(sys.argv)>1 else ROOT/'workloads-read-phases.json').read_text())
    assert report['complete'] and report['workloads_complete'] and not report['cleanup_errors']
    control_path = ROOT/'workloads-read-control.json'
    if len(sys.argv) == 1 and control_path.exists():
        control = json.loads(control_path.read_text())
        assert control['complete'] and control['workloads_complete'] and not control['cleanup_errors']
        for key in ('source_sha256','build_identity','binaries','server_sha256','snapshot','workload_fixture'):
            assert report[key] == control[key], key
        assert report['configuration']['native_trace_read_ranges']
        assert not control['configuration']['native_trace_read_ranges']
        for candidate in (report, control):
            rows = candidate['workload_trials']
            expected = {(r,p,n,b) for r in range(3) for p in ('shared','distinct') for n in (1,8,15,16,17)
                        for b in ('native-casita-repository','fuser-casita-repository','host')}
            assert len(rows) == len(expected)
            assert {(r['repetition'],r['pattern'],r['workers'],r['implementation']) for r in rows} == expected
            assert all(r['correctness'] == r['teardown'] == 'passed' for r in rows)
            assert all(len(r[p]['samples']) == r['workers'] and all(s['correctness']=='passed' for s in r[p]['samples'])
                       for r in rows for p in ('first','repeat'))
        for pattern in ('shared','distinct'):
            for count in (1,8,15,16,17):
                selected = [[r for r in c['workload_trials'] if r['pattern']==pattern and r['workers']==count
                             and r['implementation']=='native-casita-repository'] for c in (control,report)]
                print('native median first ms',pattern,count,
                      dict(zip(('untraced','traced'),(round(median(r['first']['wall_ns'] for r in s)/1e6,3) for s in selected))))
        layout = json.loads((ROOT/'workloads-macho.json').read_text())
        assert {x['sha256'] for x in layout} == {v['sha256'] for v in report['workload_fixture']['tools'].values()}
        current = {p:hashlib.sha256((ROOT.parents[2]/p).read_bytes()).hexdigest() for p in report['source_sha256']}
        print('Current sources match:', current == report['source_sha256'])
    native = [r for r in report['workload_trials'] if r['implementation']=='native-casita-repository']
    for row in native:
        values = ranges(row,'first')
        calls = sum(v[0] for v in values.values())
        assert calls == row['counters_first']['reads']-row['counters_before']['reads']
        print(row['repetition'], row['pattern'], row['workers'], 'calls',calls,
              'service_ms', round(sum(v[2] for v in values.values())/1e6,3),
              'wall_ms',round(row['first']['wall_ns']/1e6,3),
              'repeat_calls',sum(v[0] for v in ranges(row,'repeat').values())-calls)
        after = row['counters_first']['native_read_trace']
        before = row['counters_before']['native_read_trace']
        print('  phases_ms', {k:round((v-before[k])/1e6,3) for k,v in after.items() if k.endswith('_ns')})
    baseline = ranges(next(r for r in native if r['pattern']=='shared' and r['workers']==1),'first')
    for row in native:
        if row['pattern']!='shared' or row['workers']==1:
            continue
        print('\nShared',row['workers'],'extra ranges versus one worker:')
        for key,value in sorted(ranges(row,'first').items()):
            extra = value[0]-baseline.get(key,[0]*4)[0]
            if extra:
                print(key, 'extra_calls',extra, 'per_added_worker',extra/(row['workers']-1),
                      'total_service_ms',round(value[2]/1e6,3))
