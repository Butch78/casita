# Git import profile

Complete: True

Reference fd2f363; RAM-backed destination; shared host.

| Shape | Operation | Objects | Wall, s | Header+decode, s | Stage poll+drain, s | Checkpoints, s | RSS at import end, MiB |
|---|---|---:|---:|---:|---:|---:|---:|
| many-objects | initial-import | 472 | 0.186 | 0.006 | 0.111 | 0.027 | 57.4 |
| many-objects | unchanged-import | 472 | 0.011 | 0.000 | 0.000 | 0.000 | 41.8 |
| many-objects | incremental-import | 494 | 0.057 | 0.000 | 0.012 | 0.019 | 50.7 |
| many-objects | initial-import | 472 | 0.158 | 0.006 | 0.116 | 0.017 | 56.4 |
| many-objects | unchanged-import | 472 | 0.023 | 0.000 | 0.000 | 0.000 | 41.5 |
| many-objects | incremental-import | 494 | 0.061 | 0.000 | 0.006 | 0.017 | 51.5 |
| many-objects | initial-import | 472 | 0.202 | 0.006 | 0.134 | 0.023 | 57.3 |
| many-objects | unchanged-import | 472 | 0.045 | 0.000 | 0.000 | 0.000 | 41.6 |
| many-objects | incremental-import | 494 | 0.085 | 0.000 | 0.032 | 0.005 | 51.2 |
| delta-heavy | initial-import | 246 | 0.190 | 0.010 | 0.144 | 0.010 | 63.0 |
| delta-heavy | unchanged-import | 246 | 0.017 | 0.000 | 0.000 | 0.000 | 41.8 |
| delta-heavy | incremental-import | 261 | 0.061 | 0.001 | 0.011 | 0.019 | 49.4 |
| delta-heavy | initial-import | 246 | 0.217 | 0.010 | 0.178 | 0.010 | 67.4 |
| delta-heavy | unchanged-import | 246 | 0.038 | 0.000 | 0.000 | 0.000 | 41.5 |
| delta-heavy | incremental-import | 261 | 0.021 | 0.001 | 0.009 | 0.003 | 51.4 |
| delta-heavy | initial-import | 246 | 0.197 | 0.010 | 0.165 | 0.009 | 63.7 |
| delta-heavy | unchanged-import | 246 | 0.019 | 0.000 | 0.000 | 0.000 | 42.5 |
| delta-heavy | incremental-import | 261 | 0.057 | 0.001 | 0.009 | 0.024 | 45.7 |
| wide-tree | initial-import | 2118 | 0.824 | 0.020 | 0.650 | 0.079 | 89.5 |
| wide-tree | unchanged-import | 2118 | 0.057 | 0.000 | 0.000 | 0.000 | 46.7 |
| wide-tree | incremental-import | 2185 | 0.160 | 0.001 | 0.042 | 0.053 | 64.0 |
| wide-tree | initial-import | 2118 | 0.944 | 0.019 | 0.702 | 0.151 | 86.5 |
| wide-tree | unchanged-import | 2118 | 0.082 | 0.000 | 0.000 | 0.000 | 47.8 |
| wide-tree | incremental-import | 2185 | 0.179 | 0.001 | 0.040 | 0.094 | 68.3 |
| wide-tree | initial-import | 2118 | 0.742 | 0.018 | 0.588 | 0.076 | 85.6 |
| wide-tree | unchanged-import | 2118 | 0.044 | 0.000 | 0.000 | 0.000 | 46.8 |
| wide-tree | incremental-import | 2185 | 0.112 | 0.001 | 0.031 | 0.033 | 65.8 |
