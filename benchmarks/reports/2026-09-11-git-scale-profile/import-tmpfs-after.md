# Git import profile

Complete: True

RAM-backed destination to isolate CPU and filesystem work from disk flush latency; fixed git-scale smoke fixtures; shared host; no own builds or fixture generation during measured processes.

| Shape | Operation | Objects | Wall, s | Header+decode, s | Stage poll+drain, s | Checkpoints, s | RSS at import end, MiB |
|---|---|---:|---:|---:|---:|---:|---:|
| many-objects | initial-import | 472 | 0.058 | 0.003 | 0.025 | 0.012 | 44.9 |
| many-objects | unchanged-import | 472 | 0.003 | 0.000 | 0.000 | 0.000 | 33.8 |
| many-objects | incremental-import | 494 | 0.016 | 0.000 | 0.003 | 0.004 | 43.8 |
| many-objects | initial-import | 472 | 0.062 | 0.004 | 0.029 | 0.014 | 51.1 |
| many-objects | unchanged-import | 472 | 0.005 | 0.000 | 0.000 | 0.000 | 34.0 |
| many-objects | incremental-import | 494 | 0.018 | 0.000 | 0.003 | 0.004 | 42.5 |
| many-objects | initial-import | 472 | 0.057 | 0.004 | 0.028 | 0.012 | 49.2 |
| many-objects | unchanged-import | 472 | 0.007 | 0.000 | 0.000 | 0.000 | 34.5 |
| many-objects | incremental-import | 494 | 0.019 | 0.000 | 0.003 | 0.004 | 43.8 |
| delta-heavy | initial-import | 246 | 0.065 | 0.005 | 0.045 | 0.005 | 59.6 |
| delta-heavy | unchanged-import | 246 | 0.009 | 0.000 | 0.000 | 0.000 | 33.6 |
| delta-heavy | incremental-import | 261 | 0.029 | 0.001 | 0.005 | 0.011 | 45.0 |
| delta-heavy | initial-import | 246 | 0.064 | 0.006 | 0.043 | 0.006 | 57.2 |
| delta-heavy | unchanged-import | 246 | 0.010 | 0.000 | 0.000 | 0.000 | 33.6 |
| delta-heavy | incremental-import | 261 | 0.020 | 0.001 | 0.004 | 0.003 | 44.9 |
| delta-heavy | initial-import | 246 | 0.063 | 0.006 | 0.042 | 0.006 | 58.0 |
| delta-heavy | unchanged-import | 246 | 0.008 | 0.000 | 0.000 | 0.000 | 33.9 |
| delta-heavy | incremental-import | 261 | 0.020 | 0.001 | 0.004 | 0.003 | 42.4 |
| wide-tree | initial-import | 2118 | 0.238 | 0.011 | 0.145 | 0.042 | 69.6 |
| wide-tree | unchanged-import | 2118 | 0.021 | 0.000 | 0.000 | 0.000 | 38.7 |
| wide-tree | incremental-import | 2185 | 0.051 | 0.001 | 0.011 | 0.014 | 59.3 |
| wide-tree | initial-import | 2118 | 0.229 | 0.010 | 0.139 | 0.040 | 64.9 |
| wide-tree | unchanged-import | 2118 | 0.005 | 0.000 | 0.000 | 0.000 | 35.7 |
| wide-tree | incremental-import | 2185 | 0.053 | 0.001 | 0.011 | 0.014 | 60.1 |
| wide-tree | initial-import | 2118 | 0.232 | 0.011 | 0.144 | 0.040 | 60.2 |
| wide-tree | unchanged-import | 2118 | 0.017 | 0.000 | 0.000 | 0.000 | 38.3 |
| wide-tree | incremental-import | 2185 | 0.053 | 0.001 | 0.011 | 0.015 | 58.3 |
