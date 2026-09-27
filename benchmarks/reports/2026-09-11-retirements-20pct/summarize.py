import json
from pathlib import Path
from statistics import median
root = Path(__file__).resolve().parent
rows = [json.loads(p.read_text()) for p in sorted(root.glob('[36]*-*-*.json'))]
pairs = json.loads((root / 'pairs.json').read_text())
summary = dict(completed_runs=len(rows), correctness_passed=sum(r['complete'] for r in rows),
               within_cpu_ceiling=sum(r['quiet'] for r in rows), cases={})
for imports in [60,300]:
    accepted = [p['attempt'] for p in pairs if p['imports']==imports and p['accepted']]
    selected = [r for r in rows if r['imports']==imports and r['pair'] in accepted]
    case = dict(accepted_pairs=len(accepted), variants={}, paired_import_percent_changes=[])
    for variant in ['before','after']:
        samples = [r['samples'][0] for r in selected if r['variant']==variant]
        if not samples: continue
        metrics = {}
        for name,get in dict(import_seconds=lambda s:s['import_seconds'],
                             reader_p99_ms=lambda s:s['reader_open']['p99_ms'],
                             removed_during_imports=lambda s:s['gc_removed_objects_during_imports'],
                             cleanup_removed=lambda s:s['cleanup_removed_objects']).items():
            values=[get(s) for s in samples]
            metrics[name]=dict(median=median(values),min=min(values),max=max(values))
        case['variants'][variant]=metrics
    for pair in accepted:
        values={r['variant']:r['samples'][0]['import_seconds'] for r in selected if r['pair']==pair}
        case['paired_import_percent_changes'].append((values['after']/values['before']-1)*100)
    if accepted:
        case['median_paired_import_percent_change']=median(case['paired_import_percent_changes'])
    summary['cases'][imports]=case
(root/'summary.json').write_text(json.dumps(summary,indent=2))
print(json.dumps(summary,indent=2))
