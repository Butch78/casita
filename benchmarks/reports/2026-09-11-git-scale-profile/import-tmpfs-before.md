# Git import profile

Complete: True

RAM-backed destination to isolate CPU and filesystem work from disk flush latency; fixed git-scale smoke fixtures; shared host; no own builds or fixture generation during measured processes.

| Shape | Operation | Objects | Wall, s | Header+decode, s | Stage poll+drain, s | Checkpoints, s | RSS at import end, MiB |
|---|---|---:|---:|---:|---:|---:|---:|
| many-objects | initial-import | 472 | 0.223 | 0.006 | 0.158 | 0.017 | 50.5 |
| many-objects | unchanged-import | 472 | 0.014 | 0.000 | 0.000 | 0.000 | 34.8 |
| many-objects | incremental-import | 494 | 0.047 | 0.000 | 0.004 | 0.020 | 45.8 |
| many-objects | initial-import | 472 | 0.189 | 0.006 | 0.142 | 0.018 | 51.2 |
| many-objects | unchanged-import | 472 | 0.012 | 0.000 | 0.000 | 0.000 | 35.1 |
| many-objects | incremental-import | 494 | 0.032 | 0.000 | 0.004 | 0.006 | 45.6 |
| many-objects | initial-import | 472 | 0.207 | 0.006 | 0.157 | 0.018 | 52.5 |
| many-objects | unchanged-import | 472 | 0.014 | 0.000 | 0.000 | 0.000 | 35.1 |
| many-objects | incremental-import | 494 | 0.034 | 0.000 | 0.004 | 0.013 | 45.1 |
| delta-heavy | initial-import | 246 | 0.126 | 0.008 | 0.094 | 0.008 | 57.9 |
| delta-heavy | unchanged-import | 246 | 0.008 | 0.000 | 0.000 | 0.000 | 32.4 |
| delta-heavy | incremental-import | 261 | 0.022 | 0.001 | 0.004 | 0.003 | 43.5 |
| delta-heavy | initial-import | 246 | 0.116 | 0.008 | 0.085 | 0.009 | 57.0 |
| delta-heavy | unchanged-import | 246 | 0.007 | 0.000 | 0.000 | 0.000 | 34.8 |
| delta-heavy | incremental-import | 261 | 0.018 | 0.001 | 0.005 | 0.004 | 42.0 |
| delta-heavy | initial-import | 246 | 0.128 | 0.008 | 0.094 | 0.010 | 56.5 |
| delta-heavy | unchanged-import | 246 | 0.006 | 0.000 | 0.000 | 0.000 | 34.8 |
| delta-heavy | incremental-import | 261 | 0.018 | 0.001 | 0.004 | 0.003 | 43.9 |
| wide-tree | initial-import | 2118 | 2.897 | 0.021 | 2.735 | 0.062 | 71.1 |
| wide-tree | unchanged-import | 2118 | 0.024 | 0.000 | 0.000 | 0.000 | 40.1 |
| wide-tree | incremental-import | 2185 | 0.096 | 0.001 | 0.022 | 0.022 | 62.6 |
| wide-tree | initial-import | 2118 | 3.619 | 0.024 | 3.418 | 0.086 | 71.4 |
| wide-tree | unchanged-import | 2118 | 0.034 | 0.000 | 0.000 | 0.000 | 40.0 |
| wide-tree | incremental-import | 2185 | 0.107 | 0.001 | 0.017 | 0.039 | 61.9 |
| wide-tree | initial-import | 2118 | 3.844 | 0.026 | 3.661 | 0.066 | 73.7 |
| wide-tree | unchanged-import | 2118 | 0.027 | 0.000 | 0.000 | 0.000 | 39.2 |
| wide-tree | incremental-import | 2185 | 0.118 | 0.001 | 0.021 | 0.025 | 61.8 |
