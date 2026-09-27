"""Validate mounted traces and summarize first-launch service costs."""
import json
from pathlib import Path
from statistics import median

ROOT=Path(__file__).resolve().parent
r=json.loads((ROOT/'workloads-stable-cache-phases.json').read_text())
assert r['complete'] and r['workloads_complete'] and r['comparison_complete'] and not r['cleanup_errors']
rows=r['workload_trials']
expected={(rep,p,n,b) for rep in range(3) for p in ('shared','distinct') for n in (1,8,15,16,17)
          for b in ('native-casita-repository','fuser-casita-repository','host')}
assert len(rows)==len(expected)
assert {(s['repetition'],s['pattern'],s['workers'],s['implementation']) for s in rows}==expected
for s in rows:
    assert s['correctness']==s['teardown']=='passed'
    for phase in ('first','repeat'):
        assert len(s[phase]['samples'])==s['workers']
        assert all(t['correctness']=='passed' for t in s[phase]['samples'])
    if s['implementation']=='native-casita-repository':
        for phase in ('before','first','repeat'):
            counters=s['counters_'+phase]; trace=counters['native_read_trace']
            assert trace['enabled'] and trace['dropped']==0
            assert sum(v[0] for v in trace['ranges'].values())==counters['reads']
            assert all(v[3]==0 for v in trace['ranges'].values())
        assert s['native_teardown']['stats']['native_reader_cache']['resident']==0
        a,b=s['counters_before'],s['counters_first']
        t,u=a['native_read_trace'],b['native_read_trace']
        s['phases']={k:(u[k]-t[k])/1e6 for k in ('reader_lock_ns','seek_ns','stream_read_ns')}
        s['phases'].update(read_service_ms=(sum(v[2] for v in u['ranges'].values())-sum(v[2] for v in t['ranges'].values()))/1e6,
                          directory_ms=(b['directory_ns']-a['directory_ns'])/1e6,
                          open_ms=(b['open_ns']-a['open_ns'])/1e6,
                          reads=b['reads']-a['reads'],bytes=b['bytes']-a['bytes'])
for p in ('shared','distinct'):
    for n in (1,8,15,16,17):
        native=[s for s in rows if (s['pattern'],s['workers'],s['implementation'])==(p,n,'native-casita-repository')]
        metrics={k:round(median(s['phases'][k] for s in native),3) for k in native[0]['phases']}
        metrics.update({b:round(median(s['first']['wall_ns']/1e6 for s in rows if (s['pattern'],s['workers'],s['implementation'])==(p,n,b)),3)
                        for b in ('native-casita-repository','fuser-casita-repository','host')})
        print(p,n,metrics)
print('one-minute load range',min(s['load'][0] for s in rows),max(s['load'][0] for s in rows))
print('source differences from stable cache report',sorted(p for p,h in r['source_sha256'].items()
    if json.loads((ROOT.parent/'2026-09-19-stable-decoded-cache/workloads-stable-cache.json').read_text())['source_sha256'].get(p)!=h))
