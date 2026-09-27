"""Validate complete phase-change replay matrices and print medians."""
import hashlib
import json
from pathlib import Path
from statistics import median
from benchmarks.suites.decoded_seek_replay import CASES, parse_sample

ROOT=Path(__file__).resolve().parent
for name,reps in [('cache-phase-awk.json',3),('cache-phase-synthetic.json',1)]:
    r=json.loads((ROOT/name).read_text())
    expected={(rep,case,cap) for rep in range(reps) for case in CASES for cap in (0,2097152)}
    assert r['complete'] and not r.get('error')
    assert len(r['samples'])==len(expected)
    assert {(s['repetition'],s['case'],s['capacity']) for s in r['samples']}==expected
    for s in r['samples']:
        parse_sample('seek_replay_sample '+json.dumps(s)+'\n1 passed;',s['case'],s['capacity'],s['cycles'])
        assert s['chunk_range_requests']==0
    assert all(hashlib.sha256((ROOT.parents[2]/p).read_bytes()).hexdigest()==digest for p,digest in r['source_sha256'].items())
    print(name)
    for case in CASES:
        for capacity in (0,2097152):
            rows=[s for s in r['samples'] if (s['case'],s['capacity'])==(case,capacity)]
            print(case,capacity,{k:round(median(s[k] for s in rows)/(1e6 if k.endswith('_ns') else 1),4)
                  for k in ('elapsed_ns','warm_elapsed_ns','decode_calls','decode_ns','cache_bytes','warm_cache_bytes')})
