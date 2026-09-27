"""Audit and summarize the complete four-repetition CPU-only comparison."""
import collections
import hashlib
import json
from pathlib import Path
import statistics

root = Path(__file__).resolve().parent
result = json.loads((root / 'paired.json').read_text())
assert result['complete'] and len(result['samples']) == 48
assert result['configuration']['max_external_cpu_percent'] == 40
assert result['configuration']['allow_competing_builds'] is True
assert result['configuration']['file_counts'] == [4096]
assert result['configuration']['import_profile'] is False
assert result['configuration']['pin_timing'] is False
assert len(result['host_activity']) == 8
assert all(case['status'] == 'accepted' for case in result['host_activity'])
activity = [sample for case in result['host_activity'] for sample in case['measurement']['samples']]
assert all(sample['external_cpu_fraction'] <= 0.4 for sample in activity)
groups = collections.defaultdict(lambda: collections.defaultdict(dict))
digests = set()
for sample in result['samples']:
    assert sample['status'] == 'ok'
    if 'archive_report' in sample:
        digests.add(sample['archive_report']['stats']['archive_digest'])
    if sample['operation'] == 'import':
        groups[sample['seeded_file_percent']][sample['variant']][sample['repetition']] = sample['wall_seconds']
assert len(digests) == 1
assert set(groups) == {0, 50, 100}
assert sum(bool(s.get('expected_failure')) for s in result['samples']) == 8
rows = []
for reuse, variants in sorted(groups.items()):
    baseline, candidate = variants['baseline'], variants['candidate']
    assert set(baseline) == set(candidate) == {1, 2, 3, 4}
    before, after = statistics.median(baseline.values()), statistics.median(candidate.values())
    rows.append(dict(reuse_percent=reuse, baseline_seconds=baseline, candidate_seconds=candidate,
                     baseline_median=before, candidate_median=after, speedup=before/after,
                     faster_pairs=sum(candidate[k] < baseline[k] for k in baseline)))
artifacts = json.loads((root / 'artifacts.json').read_text())
assert result['configuration']['binary_sha256'] == artifacts['binaries']['candidate']['sha256']
assert result['configuration']['baseline']['sha256'] == artifacts['binaries']['baseline']['sha256']
for binary in artifacts['binaries'].values():
    with Path(binary['path']).open('rb') as handle:
        assert hashlib.file_digest(handle, 'sha256').hexdigest() == binary['sha256']
summary = dict(accepted_samples=48, expected_corruption_rejections=sum(bool(s.get('expected_failure')) for s in result['samples']),
               measurement_intervals=len(activity), peak_external_cpu_percent=max(s['external_cpu_fraction'] for s in activity)*100,
               intervals_with_compilers=sum(bool(s['competing_processes']) for s in activity), rows=rows)
(root / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
print(json.dumps(summary, indent=2))
