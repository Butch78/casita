# Git ingestion concurrency

Complete: True

Release baseline 4be9276; warm source cache; shared host; no builds launched by this investigation during measurement.

| Layout | Files | Concurrency | Byte budget | Operation | Repetition | Seconds |
|---|---:|---:|---:|---|---:|---:|
| loose | 64 | 1 | 67108864 | initial-import | 2 | 1.722948 |
| loose | 64 | 1 | 67108864 | incremental-import | 2 | 0.187014 |
| delta | 64 | 1 | 67108864 | initial-import | 1 | 0.816848 |
| delta | 64 | 1 | 67108864 | incremental-import | 1 | 0.755430 |
| packed | 64 | 1 | 67108864 | initial-import | 2 | 0.335296 |
| packed | 64 | 1 | 67108864 | incremental-import | 2 | 0.152131 |
| delta | 64 | 1 | 67108864 | initial-import | 2 | 0.699444 |
| delta | 64 | 1 | 67108864 | incremental-import | 2 | 0.232810 |
| loose | 64 | 1 | 67108864 | initial-import | 1 | 0.292925 |
| loose | 64 | 1 | 67108864 | incremental-import | 1 | 0.148641 |
| packed | 64 | 1 | 67108864 | initial-import | 0 | 0.270020 |
| packed | 64 | 1 | 67108864 | incremental-import | 0 | 0.144660 |
| packed | 64 | 1 | 67108864 | initial-import | 1 | 0.279061 |
| packed | 64 | 1 | 67108864 | incremental-import | 1 | 0.148142 |
| delta | 64 | 1 | 67108864 | initial-import | 0 | 0.692251 |
| delta | 64 | 1 | 67108864 | incremental-import | 0 | 0.240416 |
| loose | 64 | 1 | 67108864 | initial-import | 0 | 0.286412 |
| loose | 64 | 1 | 67108864 | incremental-import | 0 | 0.162099 |
