"""Validate both native reader-capacity matrices and compare service counts."""
import hashlib
import json
from pathlib import Path
from statistics import median
from benchmarks.suites.native_fskit_workloads import COUNTS, validate

ROOT=Path(__file__).resolve().parent
BACKENDS=('native-casita-repository','fuser-casita-repository','host')
reports=[]
for capacity,counts in ((16,(17,)),(32,COUNTS)):
    r=json.loads((ROOT/f'workloads-adaptive-{capacity}.json').read_text())
    assert r['complete'] and r['comparison_complete'] and r['workloads_complete'] and not r['cleanup_errors']
    assert r['configuration']['native_reader_cache_capacity']==capacity
    validate(r['workload_trials'],3,BACKENDS,counts)
    for s in r['workload_trials']:
        if s['implementation']!=BACKENDS[0]: continue
        a,b=s['counters_before'],s['counters_first']
        for phase in ('before','first','repeat'):
            c=s['counters_'+phase]; t=c['native_read_trace']; cache=c['native_reader_cache']
            assert t['enabled'] and t['dropped']==0
            assert sum(v[0] for v in t['ranges'].values())==c['reads']
            assert all(v[3]==0 for v in t['ranges'].values())
            assert cache['capacity']==capacity and cache['resident']<=capacity
        assert s['native_teardown']['stats']['native_reader_cache']['resident']==0
        t,u=a['native_read_trace'],b['native_read_trace']
        s['metrics']={k.removesuffix('_ns')+'_ms':(u[k]-t[k])/1e6 for k in ('reader_lock_ns','seek_ns','stream_read_ns')}
        s['metrics'].update(opens=b['blob_opens']-a['blob_opens'],
            evictions=b['native_reader_cache']['evictions']-a['native_reader_cache']['evictions'],
            read_ms=(sum(v[2] for v in u['ranges'].values())-sum(v[2] for v in t['ranges'].values()))/1e6,
            open_ms=(b['open_ns']-a['open_ns'])/1e6)
        last=max(s['first']['samples'],key=lambda child:child['finish_ns'])
        s['metrics'].update(last_child_total_ms=last['total_ns']/1e6,last_child_spawn_ms=last['spawn_ns']/1e6)
        if capacity==32 and s['pattern']=='distinct' and s['workers']<=32:
            assert s['metrics']['opens']==s['workers'] and s['metrics']['evictions']==0
    assert all(hashlib.sha256((ROOT.parents[2]/p).read_bytes()).hexdigest()==h for p,h in r['source_sha256'].items())
    reports.append(r)
    print('capacity',capacity,'load range',min(s['load'][0] for s in r['workload_trials']),max(s['load'][0] for s in r['workload_trials']))
    for pattern in ('shared','distinct'):
        for n in counts:
            native=[s for s in r['workload_trials'] if (s['pattern'],s['workers'],s['implementation'])==(pattern,n,BACKENDS[0])]
            metrics={key:round(median(s['metrics'][key] for s in native),3) for key in native[0]['metrics']}
            metrics.update({b:round(median(s['first']['wall_ns'] for s in r['workload_trials'] if (s['pattern'],s['workers'],s['implementation'])==(pattern,n,b))/1e6,3) for b in BACKENDS})
            print(pattern,n,metrics)
assert reports[0]['source_sha256']==reports[1]['source_sha256']
assert reports[0]['workload_fixture']==reports[1]['workload_fixture'] and reports[0]['snapshot']==reports[1]['snapshot']
assert reports[0]['binaries']==reports[1]['binaries'] and reports[0]['server_sha256']==reports[1]['server_sha256']
assert reports[0]['build_identity']==reports[1]['build_identity']
print('Same source, bundle, server and fixture verified.')
