# Git import profile

Complete: True

Reference fd2f363 plus cached checkpoint index reuse; tmpfs destination; shared host.

| Shape | Operation | Objects | Wall, s | Header+decode, s | Stage poll+drain, s | Checkpoints, s | RSS at import end, MiB |
|---|---|---:|---:|---:|---:|---:|---:|
| many-objects | initial-import | 472 | 0.057 | 0.003 | 0.025 | 0.011 | 57.5 |
| many-objects | unchanged-import | 472 | 0.004 | 0.000 | 0.000 | 0.000 | 42.4 |
| many-objects | incremental-import | 494 | 0.017 | 0.000 | 0.003 | 0.005 | 52.0 |
| many-objects | initial-import | 472 | 0.044 | 0.003 | 0.022 | 0.008 | 58.2 |
| many-objects | unchanged-import | 472 | 0.005 | 0.000 | 0.000 | 0.000 | 43.2 |
| many-objects | incremental-import | 494 | 0.020 | 0.000 | 0.003 | 0.007 | 52.6 |
| many-objects | initial-import | 472 | 0.045 | 0.003 | 0.023 | 0.008 | 55.1 |
| many-objects | unchanged-import | 472 | 0.003 | 0.000 | 0.000 | 0.000 | 42.8 |
| many-objects | incremental-import | 494 | 0.014 | 0.000 | 0.003 | 0.003 | 52.7 |
| delta-heavy | initial-import | 246 | 0.053 | 0.005 | 0.034 | 0.005 | 61.9 |
| delta-heavy | unchanged-import | 246 | 0.007 | 0.000 | 0.000 | 0.000 | 42.5 |
| delta-heavy | incremental-import | 261 | 0.017 | 0.000 | 0.004 | 0.002 | 53.7 |
| delta-heavy | initial-import | 246 | 0.055 | 0.004 | 0.036 | 0.006 | 61.2 |
| delta-heavy | unchanged-import | 246 | 0.009 | 0.000 | 0.000 | 0.000 | 43.0 |
| delta-heavy | incremental-import | 261 | 0.017 | 0.000 | 0.003 | 0.003 | 52.4 |
| delta-heavy | initial-import | 246 | 0.053 | 0.004 | 0.036 | 0.005 | 65.5 |
| delta-heavy | unchanged-import | 246 | 0.006 | 0.000 | 0.000 | 0.000 | 42.9 |
| delta-heavy | incremental-import | 261 | 0.015 | 0.000 | 0.003 | 0.003 | 53.6 |
| wide-tree | initial-import | 2118 | 0.182 | 0.009 | 0.110 | 0.033 | 66.5 |
| wide-tree | unchanged-import | 2118 | 0.005 | 0.000 | 0.000 | 0.000 | 45.1 |
| wide-tree | incremental-import | 2185 | 0.039 | 0.001 | 0.008 | 0.011 | 63.4 |
| wide-tree | initial-import | 2118 | 0.199 | 0.009 | 0.129 | 0.031 | 67.3 |
| wide-tree | unchanged-import | 2118 | 0.018 | 0.000 | 0.000 | 0.000 | 47.6 |
| wide-tree | incremental-import | 2185 | 0.040 | 0.000 | 0.009 | 0.011 | 62.8 |
| wide-tree | initial-import | 2118 | 0.195 | 0.009 | 0.124 | 0.032 | 69.1 |
| wide-tree | unchanged-import | 2118 | 0.004 | 0.000 | 0.000 | 0.000 | 44.5 |
| wide-tree | incremental-import | 2185 | 0.040 | 0.000 | 0.009 | 0.012 | 66.7 |
