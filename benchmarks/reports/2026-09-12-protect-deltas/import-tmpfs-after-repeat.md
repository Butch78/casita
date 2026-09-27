# Git import profile

Complete: True

Candidate repeat immediately after reference repeat to check mixed smoke latencies; tmpfs destination; shared host.

| Shape | Operation | Objects | Wall, s | Header+decode, s | Stage poll+drain, s | Checkpoints, s | RSS at import end, MiB |
|---|---|---:|---:|---:|---:|---:|---:|
| many-objects | initial-import | 472 | 0.042 | 0.003 | 0.018 | 0.009 | 45.4 |
| many-objects | unchanged-import | 472 | 0.006 | 0.000 | 0.000 | 0.000 | 34.4 |
| many-objects | incremental-import | 494 | 0.014 | 0.000 | 0.002 | 0.003 | 44.0 |
| many-objects | initial-import | 472 | 0.038 | 0.003 | 0.017 | 0.009 | 48.4 |
| many-objects | unchanged-import | 472 | 0.004 | 0.000 | 0.000 | 0.000 | 34.3 |
| many-objects | incremental-import | 494 | 0.014 | 0.000 | 0.003 | 0.003 | 43.3 |
| many-objects | initial-import | 472 | 0.038 | 0.003 | 0.017 | 0.010 | 45.1 |
| many-objects | unchanged-import | 472 | 0.005 | 0.000 | 0.000 | 0.000 | 34.1 |
| many-objects | incremental-import | 494 | 0.013 | 0.000 | 0.003 | 0.003 | 44.2 |
| delta-heavy | initial-import | 246 | 0.046 | 0.004 | 0.029 | 0.005 | 55.1 |
| delta-heavy | unchanged-import | 246 | 0.003 | 0.000 | 0.000 | 0.000 | 34.2 |
| delta-heavy | incremental-import | 261 | 0.012 | 0.000 | 0.003 | 0.002 | 43.9 |
| delta-heavy | initial-import | 246 | 0.052 | 0.005 | 0.034 | 0.005 | 53.2 |
| delta-heavy | unchanged-import | 246 | 0.004 | 0.000 | 0.000 | 0.000 | 33.9 |
| delta-heavy | incremental-import | 261 | 0.012 | 0.000 | 0.003 | 0.002 | 44.8 |
| delta-heavy | initial-import | 246 | 0.051 | 0.005 | 0.032 | 0.005 | 54.3 |
| delta-heavy | unchanged-import | 246 | 0.003 | 0.000 | 0.000 | 0.000 | 34.1 |
| delta-heavy | incremental-import | 261 | 0.012 | 0.000 | 0.003 | 0.002 | 45.4 |
| wide-tree | initial-import | 2118 | 0.117 | 0.008 | 0.057 | 0.030 | 55.2 |
| wide-tree | unchanged-import | 2118 | 0.011 | 0.000 | 0.000 | 0.000 | 36.5 |
| wide-tree | incremental-import | 2185 | 0.028 | 0.000 | 0.005 | 0.010 | 53.6 |
| wide-tree | initial-import | 2118 | 0.123 | 0.008 | 0.062 | 0.031 | 53.9 |
| wide-tree | unchanged-import | 2118 | 0.010 | 0.000 | 0.000 | 0.000 | 34.8 |
| wide-tree | incremental-import | 2185 | 0.028 | 0.000 | 0.005 | 0.010 | 53.7 |
| wide-tree | initial-import | 2118 | 0.115 | 0.008 | 0.056 | 0.030 | 54.6 |
| wide-tree | unchanged-import | 2118 | 0.011 | 0.000 | 0.000 | 0.000 | 36.4 |
| wide-tree | incremental-import | 2185 | 0.028 | 0.000 | 0.005 | 0.009 | 52.9 |
