# Git import profile

Complete: True

Candidate fd2f363 plus cached checkpoint index reuse; RAM-backed destination; shared host.

| Shape | Operation | Objects | Wall, s | Header+decode, s | Stage poll+drain, s | Checkpoints, s | RSS at import end, MiB |
|---|---|---:|---:|---:|---:|---:|---:|
| many-objects | initial-import | 472 | 0.057 | 0.004 | 0.025 | 0.012 | 49.7 |
| many-objects | unchanged-import | 472 | 0.007 | 0.000 | 0.000 | 0.000 | 34.2 |
| many-objects | incremental-import | 494 | 0.021 | 0.000 | 0.003 | 0.005 | 44.7 |
| many-objects | initial-import | 472 | 0.052 | 0.004 | 0.025 | 0.010 | 47.8 |
| many-objects | unchanged-import | 472 | 0.007 | 0.000 | 0.000 | 0.000 | 34.4 |
| many-objects | incremental-import | 494 | 0.019 | 0.000 | 0.003 | 0.005 | 44.9 |
| many-objects | initial-import | 472 | 0.053 | 0.004 | 0.026 | 0.011 | 45.2 |
| many-objects | unchanged-import | 472 | 0.006 | 0.000 | 0.000 | 0.000 | 34.1 |
| many-objects | incremental-import | 494 | 0.018 | 0.000 | 0.003 | 0.004 | 44.9 |
| delta-heavy | initial-import | 246 | 0.077 | 0.007 | 0.054 | 0.005 | 57.2 |
| delta-heavy | unchanged-import | 246 | 0.011 | 0.000 | 0.000 | 0.000 | 33.9 |
| delta-heavy | incremental-import | 261 | 0.024 | 0.001 | 0.004 | 0.003 | 45.2 |
| delta-heavy | initial-import | 246 | 0.067 | 0.006 | 0.045 | 0.007 | 57.8 |
| delta-heavy | unchanged-import | 246 | 0.010 | 0.000 | 0.000 | 0.000 | 33.3 |
| delta-heavy | incremental-import | 261 | 0.019 | 0.001 | 0.004 | 0.003 | 45.0 |
| delta-heavy | initial-import | 246 | 0.065 | 0.006 | 0.044 | 0.006 | 57.5 |
| delta-heavy | unchanged-import | 246 | 0.013 | 0.000 | 0.000 | 0.000 | 33.4 |
| delta-heavy | incremental-import | 261 | 0.021 | 0.001 | 0.004 | 0.003 | 43.4 |
| wide-tree | initial-import | 2118 | 0.256 | 0.011 | 0.162 | 0.043 | 60.8 |
| wide-tree | unchanged-import | 2118 | 0.023 | 0.000 | 0.000 | 0.000 | 38.7 |
| wide-tree | incremental-import | 2185 | 0.052 | 0.001 | 0.011 | 0.012 | 54.6 |
| wide-tree | initial-import | 2118 | 0.218 | 0.010 | 0.138 | 0.036 | 62.6 |
| wide-tree | unchanged-import | 2118 | 0.017 | 0.000 | 0.000 | 0.000 | 38.5 |
| wide-tree | incremental-import | 2185 | 0.043 | 0.001 | 0.010 | 0.011 | 58.2 |
| wide-tree | initial-import | 2118 | 0.193 | 0.010 | 0.114 | 0.036 | 59.5 |
| wide-tree | unchanged-import | 2118 | 0.006 | 0.000 | 0.000 | 0.000 | 35.8 |
| wide-tree | incremental-import | 2185 | 0.043 | 0.001 | 0.010 | 0.013 | 57.7 |
