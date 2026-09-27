"""Validate complete replay matrices and summarize the mounted comparison."""
import hashlib
import json
from pathlib import Path
from statistics import median

ROOT=Path(__file__).resolve().parent
CASES=('launch','sequential','one-seek','two-seeks','chunk-below','chunk-at','chunk-above',
       'working-below','working-at','working-above','entries-below','entries-at','entries-above')


def read(name): return json.loads((ROOT/name).read_text())


def replay(name,repetitions):
    r=read(name)
    assert r['complete'] and not r.get('error')
    expected={(rep,case,cap) for rep in range(repetitions) for case in CASES for cap in (0,2097152)}
    assert len(r['samples'])==len(expected)
    assert {(s['repetition'],s['case'],s['capacity']) for s in r['samples']}==expected
    for s in r['samples']:
        assert s['correctness']==s['release']=='passed'
        assert s['cache_bytes']<=s['capacity'] and s['cache_entries']<=64
        assert s['chunk_range_requests']==0
    print(name)
    for case in CASES:
        for cap in (0,2097152):
            selected=[s for s in r['samples'] if s['case']==case and s['capacity']==cap]
            print(case,cap,{k:round(median(s[k] for s in selected)/(1e6 if k.endswith('_ns') else 1),4)
                  for k in ('elapsed_ns','decode_calls','decoded_bytes','fetch_ns','admission_ns','decode_ns','cache_bytes','cache_entries')})
    current={p:hashlib.sha256((ROOT.parents[2]/p).read_bytes()).hexdigest() for p in r['source_sha256']}
    print('Current sources match:',current==r['source_sha256'])
    return r


if __name__=='__main__':
    real=replay('decoded-seek-awk.json',3)
    synthetic=replay('decoded-seek-synthetic.json',1)
    assert real['artifacts']==synthetic['artifacts'] and real['source_sha256']==synthetic['source_sha256']
    if (ROOT/'workloads-decoded-cache-retry.json').exists():
        new=read('workloads-decoded-cache-retry.json')
        old=json.loads((ROOT.parent/'2026-09-18-native-fskit-read-ranges/workloads-read-control.json').read_text())
        for r in (old,new):
            assert r['complete'] and r['comparison_complete'] and r['workloads_complete'] and not r['cleanup_errors']
            rows=r['workload_trials']
            assert len(rows)==90
            expected={(rep,p,n,b) for rep in range(3) for p in ('shared','distinct') for n in (1,8,15,16,17)
                      for b in ('native-casita-repository','fuser-casita-repository','host')}
            assert {(s['repetition'],s['pattern'],s['workers'],s['implementation']) for s in rows}==expected
            for s in rows:
                assert s['correctness']==s['teardown']=='passed'
                for phase in ('first','repeat'):
                    assert len(s[phase]['samples'])==s['workers']
                    assert all(t['correctness']=='passed' for t in s[phase]['samples'])
                if s['implementation']=='native-casita-repository':
                    assert s['native_teardown']['stats']['native_reader_cache']['resident']==0
        assert old['workload_fixture']==new['workload_fixture'] and old['snapshot']==new['snapshot']
        changed={p for p in old['source_sha256'].keys()|new['source_sha256'].keys()
                 if old['source_sha256'].get(p)!=new['source_sha256'].get(p)}
        assert changed=={'src/blob/chunked_reader.rs','src/blob/pack/fetch.rs','src/blob/pack/seek_replay.rs'},changed
        print('Native source changes:',sorted(changed))
        for p in ('shared','distinct'):
            for n in (1,8,15,16,17):
                print(p,n)
                for backend in ('native-casita-repository','fuser-casita-repository','host'):
                    selected=[[s for s in r['workload_trials'] if (s['pattern'],s['workers'],s['implementation'])==(p,n,backend)] for r in (old,new)]
                    print(backend,'first_ms',[round(median(s['first']['wall_ns'] for s in rows)/1e6,3) for rows in selected],
                          'repeat_ms',[round(median(s['repeat']['wall_ns'] for s in rows)/1e6,3) for rows in selected])
