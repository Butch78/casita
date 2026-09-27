# Git import profile

Complete: True

Reference repeat after candidate measurements to check mixed smoke latencies; tmpfs destination; shared host.

| Shape | Operation | Objects | Wall, s | Header+decode, s | Stage poll+drain, s | Checkpoints, s | RSS at import end, MiB |
|---|---|---:|---:|---:|---:|---:|---:|
| many-objects | initial-import | 472 | 0.054 | 0.003 | 0.025 | 0.010 | 52.6 |
| many-objects | unchanged-import | 472 | 0.007 | 0.000 | 0.000 | 0.000 | 37.0 |
| many-objects | incremental-import | 494 | 0.017 | 0.000 | 0.003 | 0.004 | 46.7 |
| many-objects | initial-import | 472 | 0.043 | 0.003 | 0.020 | 0.009 | 50.7 |
| many-objects | unchanged-import | 472 | 0.005 | 0.000 | 0.000 | 0.000 | 36.2 |
| many-objects | incremental-import | 494 | 0.018 | 0.000 | 0.003 | 0.003 | 46.6 |
| many-objects | initial-import | 472 | 0.049 | 0.003 | 0.024 | 0.010 | 51.3 |
| many-objects | unchanged-import | 472 | 0.003 | 0.000 | 0.000 | 0.000 | 35.1 |
| many-objects | incremental-import | 494 | 0.014 | 0.000 | 0.003 | 0.003 | 46.1 |
| delta-heavy | initial-import | 246 | 0.055 | 0.004 | 0.037 | 0.005 | 61.1 |
| delta-heavy | unchanged-import | 246 | 0.008 | 0.000 | 0.000 | 0.000 | 37.1 |
| delta-heavy | incremental-import | 261 | 0.016 | 0.000 | 0.003 | 0.002 | 46.2 |
| delta-heavy | initial-import | 246 | 0.054 | 0.004 | 0.037 | 0.005 | 60.4 |
| delta-heavy | unchanged-import | 246 | 0.007 | 0.000 | 0.000 | 0.000 | 36.2 |
| delta-heavy | incremental-import | 261 | 0.016 | 0.000 | 0.003 | 0.002 | 47.3 |
| delta-heavy | initial-import | 246 | 0.053 | 0.005 | 0.036 | 0.005 | 60.7 |
| delta-heavy | unchanged-import | 246 | 0.008 | 0.000 | 0.000 | 0.000 | 36.2 |
| delta-heavy | incremental-import | 261 | 0.016 | 0.000 | 0.003 | 0.003 | 46.8 |
| wide-tree | initial-import | 2118 | 0.160 | 0.009 | 0.090 | 0.030 | 61.2 |
| wide-tree | unchanged-import | 2118 | 0.013 | 0.000 | 0.000 | 0.000 | 41.0 |
| wide-tree | incremental-import | 2185 | 0.037 | 0.000 | 0.008 | 0.011 | 56.2 |
| wide-tree | initial-import | 2118 | 0.182 | 0.008 | 0.112 | 0.030 | 64.8 |
| wide-tree | unchanged-import | 2118 | 0.005 | 0.000 | 0.000 | 0.000 | 38.5 |
| wide-tree | incremental-import | 2185 | 0.040 | 0.000 | 0.008 | 0.011 | 57.2 |
| wide-tree | initial-import | 2118 | 0.200 | 0.008 | 0.129 | 0.032 | 57.9 |
| wide-tree | unchanged-import | 2118 | 0.005 | 0.000 | 0.000 | 0.000 | 37.8 |
| wide-tree | incremental-import | 2185 | 0.039 | 0.000 | 0.008 | 0.010 | 58.0 |
