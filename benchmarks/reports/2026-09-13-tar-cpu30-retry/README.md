# Tar retry: three accepted matrices, one contended

All four 16-case matrices completed with the existing import/readback
correctness gates: 64 successful case executions. The first three matrices
passed the user-selected 30% external CPU ceiling with build activity allowed
and recorded. The fourth exceeded that ceiling after new Clippy jobs started,
so the planned four-matrix baseline remains incomplete. No replacement run
was selected to hide the rejection, and no full-study speedup is claimed.

| Matrix | Status | Median external CPU | Peak external CPU |
|---|---|---:|---:|
| 1 | accepted | 10.03% | 21.29% |
| 2 | accepted | 12.08% | 15.98% |
| 3 | accepted | 11.27% | 14.72% |
| 4 | contended | 27.91% | 52.28% |

These diagnostics summarize only the three accepted matrices. Values are
median case-mean milliseconds, with the range of case means in parentheses;
they are not confidence intervals. The fourth matrix remains in the raw report
and is excluded here under the preselected CPU policy.

| Archive shape | Concurrency 1, ms | Concurrency 16, ms |
|---|---:|---:|
| small-256 | 15.19 (15.18–15.98) | 9.07 (8.61–9.32) |
| large | 37.24 (37.23–38.53) | 32.44 (31.72–33.05) |
| mixed | 40.97 (40.18–41.08) | 33.54 (33.51–33.94) |

`small-256` contains 256 × 1 KiB files, `large` contains 4 × 4 MiB files,
and `mixed` contains 32 × 1 KiB plus 4 × 4 MiB files. Fixtures use deterministic
random bytes and in-memory payload/metadata storage. These numbers describe
this fixture and shared host, not a FastCDC version comparison or production
storage throughput. The existing source and immutable executable fingerprints
were verified again after the run.

[Retained evidence](report.json) includes all four matrices, raw Criterion
samples/estimates, activity records, benchmark logs, and binary/build provenance.
Original Criterion directories are in
`benchmarks/results/2026-09-13-tar-cpu30-retry/`. No process was paused and no
production code, CI, or billing was changed. The CPU-policy implementation was
committed locally as `1a0a6a6`; its 232 Python harness tests passed with two
skipped.

## Reproduce

```sh
python3 -m benchmarks.tar_compare \
  --binary benchmarks/results/2026-09-12-tar-baseline/tar-baseline \
  --output /tmp/tar-cpu30-retry-new --repetitions 4 --quiet-timeout 180 \
  --max-external-cpu-percent 30 --allow-competing-builds
```

Use a fresh output directory and an immutable executable built from the desired
revision. The same 16 permanent tar cases remain registered in the manifest
and included in `benchmark all`.
