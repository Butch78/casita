# Git import profile

Complete: True

Candidate with incremental staging-pin index updates; same fixed git-scale smoke sources as reference; no own builds or source generation during measurements; shared Btrfs host with storage pressure above 80 percent.

| Shape | Operation | Objects | Wall, s | Header+decode, s | Stage poll+drain, s | Checkpoints, s | RSS at import end, MiB |
|---|---|---:|---:|---:|---:|---:|---:|
| many-objects | initial-import | 472 | 2.810 | 0.008 | 2.029 | 0.212 | 49.9 |
| many-objects | unchanged-import | 472 | 0.063 | 0.000 | 0.000 | 0.000 | 35.2 |
| many-objects | incremental-import | 494 | 0.726 | 0.000 | 0.153 | 0.082 | 45.2 |
| many-objects | initial-import | 472 | 2.647 | 0.007 | 1.940 | 0.124 | 49.6 |
| many-objects | unchanged-import | 472 | 0.092 | 0.000 | 0.000 | 0.000 | 32.4 |
| many-objects | incremental-import | 494 | 0.783 | 0.000 | 0.159 | 0.101 | 44.5 |
| many-objects | initial-import | 472 | 3.927 | 0.007 | 1.772 | 0.117 | 49.3 |
| many-objects | unchanged-import | 472 | 0.049 | 0.000 | 0.000 | 0.000 | 34.8 |
| many-objects | incremental-import | 494 | 0.789 | 0.000 | 0.120 | 0.077 | 44.2 |
| delta-heavy | initial-import | 246 | 2.379 | 0.011 | 1.895 | 0.079 | 55.0 |
| delta-heavy | unchanged-import | 246 | 0.029 | 0.000 | 0.000 | 0.000 | 34.0 |
| delta-heavy | incremental-import | 261 | 0.359 | 0.001 | 0.176 | 0.061 | 44.0 |
| delta-heavy | initial-import | 246 | 2.473 | 0.009 | 2.029 | 0.101 | 54.0 |
| delta-heavy | unchanged-import | 246 | 0.232 | 0.000 | 0.000 | 0.000 | 34.3 |
| delta-heavy | incremental-import | 261 | 0.391 | 0.001 | 0.187 | 0.072 | 44.2 |
| delta-heavy | initial-import | 246 | 2.322 | 0.009 | 1.985 | 0.050 | 58.8 |
| delta-heavy | unchanged-import | 246 | 0.027 | 0.000 | 0.000 | 0.000 | 34.1 |
| delta-heavy | incremental-import | 261 | 0.404 | 0.001 | 0.153 | 0.157 | 44.7 |
| wide-tree | initial-import | 2118 | 8.171 | 0.028 | 7.533 | 0.140 | 65.4 |
| wide-tree | unchanged-import | 2118 | 0.132 | 0.000 | 0.000 | 0.000 | 38.8 |
| wide-tree | incremental-import | 2185 | 1.014 | 0.001 | 0.303 | 0.169 | 59.7 |
| wide-tree | initial-import | 2118 | 6.472 | 0.021 | 5.801 | 0.147 | 66.8 |
| wide-tree | unchanged-import | 2118 | 0.053 | 0.000 | 0.000 | 0.000 | 39.0 |
| wide-tree | incremental-import | 2185 | 0.733 | 0.001 | 0.295 | 0.153 | 58.0 |
| wide-tree | initial-import | 2118 | 6.172 | 0.027 | 5.683 | 0.127 | 68.8 |
| wide-tree | unchanged-import | 2118 | 0.039 | 0.000 | 0.000 | 0.000 | 38.5 |
| wide-tree | incremental-import | 2185 | 0.701 | 0.001 | 0.311 | 0.117 | 58.2 |
