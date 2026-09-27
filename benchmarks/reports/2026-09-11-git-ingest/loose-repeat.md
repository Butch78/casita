# Git ingestion concurrency

Complete: True

Focused repeat of noisy loose-object case; randomized serial/concurrent order, same optimized binary and fresh repositories; shared host; no builds launched by this investigation during measurement.

| Layout | Files | Concurrency | Byte budget | Operation | Repetition | Seconds |
|---|---:|---:|---:|---|---:|---:|
| loose | 64 | 16 | 67108864 | initial-import | 2 | 1.448474 |
| loose | 64 | 16 | 67108864 | incremental-import | 2 | 0.356583 |
| loose | 64 | 16 | 67108864 | initial-import | 0 | 0.586081 |
| loose | 64 | 16 | 67108864 | incremental-import | 0 | 0.325481 |
| loose | 64 | 16 | 67108864 | initial-import | 3 | 0.523401 |
| loose | 64 | 16 | 67108864 | incremental-import | 3 | 0.327312 |
| loose | 64 | 1 | 67108864 | initial-import | 3 | 1.523599 |
| loose | 64 | 1 | 67108864 | incremental-import | 3 | 0.676221 |
| loose | 64 | 1 | 67108864 | initial-import | 7 | 1.436143 |
| loose | 64 | 1 | 67108864 | incremental-import | 7 | 0.677005 |
| loose | 64 | 1 | 67108864 | initial-import | 6 | 1.472706 |
| loose | 64 | 1 | 67108864 | incremental-import | 6 | 0.678227 |
| loose | 64 | 16 | 67108864 | initial-import | 8 | 0.512221 |
| loose | 64 | 16 | 67108864 | incremental-import | 8 | 0.314788 |
| loose | 64 | 16 | 67108864 | initial-import | 7 | 0.509584 |
| loose | 64 | 16 | 67108864 | incremental-import | 7 | 0.293023 |
| loose | 64 | 1 | 67108864 | initial-import | 5 | 1.772047 |
| loose | 64 | 1 | 67108864 | incremental-import | 5 | 0.705917 |
| loose | 64 | 1 | 67108864 | initial-import | 8 | 1.514546 |
| loose | 64 | 1 | 67108864 | incremental-import | 8 | 0.656997 |
| loose | 64 | 1 | 67108864 | initial-import | 4 | 1.487811 |
| loose | 64 | 1 | 67108864 | incremental-import | 4 | 0.752065 |
| loose | 64 | 16 | 67108864 | initial-import | 6 | 0.610040 |
| loose | 64 | 16 | 67108864 | incremental-import | 6 | 0.322160 |
| loose | 64 | 1 | 67108864 | initial-import | 0 | 1.632856 |
| loose | 64 | 1 | 67108864 | incremental-import | 0 | 0.718671 |
| loose | 64 | 16 | 67108864 | initial-import | 1 | 0.534197 |
| loose | 64 | 16 | 67108864 | incremental-import | 1 | 0.343441 |
| loose | 64 | 1 | 67108864 | initial-import | 2 | 1.545409 |
| loose | 64 | 1 | 67108864 | incremental-import | 2 | 0.725999 |
| loose | 64 | 16 | 67108864 | initial-import | 5 | 0.522497 |
| loose | 64 | 16 | 67108864 | incremental-import | 5 | 0.293622 |
| loose | 64 | 16 | 67108864 | initial-import | 4 | 0.523278 |
| loose | 64 | 16 | 67108864 | incremental-import | 4 | 0.304691 |
| loose | 64 | 1 | 67108864 | initial-import | 1 | 1.489750 |
| loose | 64 | 1 | 67108864 | incremental-import | 1 | 0.763878 |
