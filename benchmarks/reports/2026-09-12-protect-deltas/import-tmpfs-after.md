# Git import profile

Complete: True

Candidate resource-addition journal frames on top of cached checkpoint index reuse; tmpfs destination; shared host.

| Shape | Operation | Objects | Wall, s | Header+decode, s | Stage poll+drain, s | Checkpoints, s | RSS at import end, MiB |
|---|---|---:|---:|---:|---:|---:|---:|
| many-objects | initial-import | 472 | 0.065 | 0.005 | 0.033 | 0.013 | 48.6 |
| many-objects | unchanged-import | 472 | 0.007 | 0.000 | 0.000 | 0.000 | 34.2 |
| many-objects | incremental-import | 494 | 0.023 | 0.000 | 0.004 | 0.005 | 42.4 |
| many-objects | initial-import | 472 | 0.090 | 0.005 | 0.049 | 0.018 | 46.7 |
| many-objects | unchanged-import | 472 | 0.005 | 0.000 | 0.000 | 0.000 | 34.2 |
| many-objects | incremental-import | 494 | 0.020 | 0.000 | 0.003 | 0.004 | 39.2 |
| many-objects | initial-import | 472 | 0.074 | 0.005 | 0.035 | 0.017 | 49.4 |
| many-objects | unchanged-import | 472 | 0.008 | 0.000 | 0.000 | 0.000 | 34.3 |
| many-objects | incremental-import | 494 | 0.021 | 0.000 | 0.004 | 0.005 | 43.9 |
| delta-heavy | initial-import | 246 | 0.081 | 0.009 | 0.055 | 0.008 | 53.8 |
| delta-heavy | unchanged-import | 246 | 0.006 | 0.000 | 0.000 | 0.000 | 33.8 |
| delta-heavy | incremental-import | 261 | 0.015 | 0.001 | 0.004 | 0.003 | 44.1 |
| delta-heavy | initial-import | 246 | 0.072 | 0.007 | 0.048 | 0.008 | 55.9 |
| delta-heavy | unchanged-import | 246 | 0.006 | 0.000 | 0.000 | 0.000 | 32.1 |
| delta-heavy | incremental-import | 261 | 0.020 | 0.001 | 0.005 | 0.003 | 44.6 |
| delta-heavy | initial-import | 246 | 0.068 | 0.007 | 0.043 | 0.007 | 55.3 |
| delta-heavy | unchanged-import | 246 | 0.007 | 0.000 | 0.000 | 0.000 | 34.2 |
| delta-heavy | incremental-import | 261 | 0.017 | 0.001 | 0.005 | 0.003 | 44.7 |
| wide-tree | initial-import | 2118 | 0.162 | 0.013 | 0.086 | 0.036 | 56.5 |
| wide-tree | unchanged-import | 2118 | 0.013 | 0.000 | 0.000 | 0.000 | 36.6 |
| wide-tree | incremental-import | 2185 | 0.033 | 0.000 | 0.005 | 0.011 | 53.1 |
| wide-tree | initial-import | 2118 | 0.129 | 0.009 | 0.064 | 0.031 | 56.7 |
| wide-tree | unchanged-import | 2118 | 0.010 | 0.000 | 0.000 | 0.000 | 36.8 |
| wide-tree | incremental-import | 2185 | 0.032 | 0.000 | 0.006 | 0.011 | 53.3 |
| wide-tree | initial-import | 2118 | 0.124 | 0.009 | 0.062 | 0.031 | 57.4 |
| wide-tree | unchanged-import | 2118 | 0.011 | 0.000 | 0.000 | 0.000 | 37.0 |
| wide-tree | incremental-import | 2185 | 0.031 | 0.000 | 0.005 | 0.010 | 53.3 |
