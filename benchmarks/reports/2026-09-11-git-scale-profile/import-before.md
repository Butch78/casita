# Git import profile

Complete: True

Production 949f145 with test-only phase instrumentation; same fixed git-scale smoke sources as candidate; no own builds or source generation during measurements; shared Btrfs host with storage pressure above 80 percent.

| Shape | Operation | Objects | Wall, s | Header+decode, s | Stage poll+drain, s | Checkpoints, s | RSS at import end, MiB |
|---|---|---:|---:|---:|---:|---:|---:|
| many-objects | initial-import | 472 | 3.017 | 0.006 | 1.371 | 0.082 | 59.2 |
| many-objects | unchanged-import | 472 | 0.035 | 0.000 | 0.000 | 0.000 | 43.7 |
| many-objects | incremental-import | 494 | 0.461 | 0.000 | 0.207 | 0.076 | 51.8 |
| many-objects | initial-import | 472 | 1.771 | 0.006 | 1.336 | 0.081 | 58.3 |
| many-objects | unchanged-import | 472 | 0.033 | 0.000 | 0.000 | 0.000 | 42.7 |
| many-objects | incremental-import | 494 | 0.380 | 0.000 | 0.116 | 0.069 | 52.0 |
| many-objects | initial-import | 472 | 1.720 | 0.006 | 1.330 | 0.068 | 58.8 |
| many-objects | unchanged-import | 472 | 0.040 | 0.000 | 0.000 | 0.000 | 42.7 |
| many-objects | incremental-import | 494 | 0.367 | 0.000 | 0.120 | 0.113 | 52.2 |
| delta-heavy | initial-import | 246 | 2.338 | 0.008 | 2.033 | 0.075 | 64.0 |
| delta-heavy | unchanged-import | 246 | 0.034 | 0.000 | 0.000 | 0.000 | 42.9 |
| delta-heavy | incremental-import | 261 | 0.418 | 0.001 | 0.178 | 0.107 | 51.6 |
| delta-heavy | initial-import | 246 | 4.572 | 0.008 | 4.122 | 0.078 | 66.7 |
| delta-heavy | unchanged-import | 246 | 0.040 | 0.000 | 0.000 | 0.000 | 42.5 |
| delta-heavy | incremental-import | 261 | 0.419 | 0.001 | 0.233 | 0.074 | 49.4 |
| delta-heavy | initial-import | 246 | 3.687 | 0.010 | 3.060 | 0.091 | 65.4 |
| delta-heavy | unchanged-import | 246 | 0.064 | 0.000 | 0.000 | 0.000 | 42.2 |
| delta-heavy | incremental-import | 261 | 0.861 | 0.001 | 0.275 | 0.179 | 50.8 |
| wide-tree | initial-import | 2118 | 17.670 | 0.025 | 15.673 | 0.212 | 80.2 |
| wide-tree | unchanged-import | 2118 | 0.066 | 0.000 | 0.000 | 0.000 | 47.6 |
| wide-tree | incremental-import | 2185 | 2.764 | 0.001 | 1.647 | 0.141 | 69.7 |
| wide-tree | initial-import | 2118 | 8.183 | 0.018 | 7.511 | 0.108 | 78.0 |
| wide-tree | unchanged-import | 2118 | 0.041 | 0.000 | 0.000 | 0.000 | 47.2 |
| wide-tree | incremental-import | 2185 | 1.019 | 0.001 | 0.301 | 0.118 | 67.6 |
| wide-tree | initial-import | 2118 | 7.531 | 0.017 | 7.052 | 0.101 | 77.2 |
| wide-tree | unchanged-import | 2118 | 0.050 | 0.000 | 0.000 | 0.000 | 48.1 |
| wide-tree | incremental-import | 2185 | 0.654 | 0.001 | 0.314 | 0.123 | 67.5 |
