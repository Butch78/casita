# Ingest scheduling

Complete: True

walk, bounded file reads and hashing; fixture setup and validation excluded; no repository publication

20 ms before every 16th file, 1 ms before others; uniform has no injected delay

Focused repeat of the noisy uniform serial control; both schedulers in one optimized binary, randomized order, shared host, no overlapping builds launched by this investigation.

| Files | Concurrency | Pattern | Mode | Repetition | Seconds | Peak reads |
|---:|---:|---|---|---:|---:|---:|
| 1024 | 1 | uniform | ready | 5 | 0.119695 | 1 |
| 1024 | 1 | uniform | ready | 4 | 0.116679 | 1 |
| 1024 | 1 | uniform | ordered | 6 | 0.114361 | 1 |
| 1024 | 1 | uniform | ready | 1 | 0.113152 | 1 |
| 1024 | 1 | uniform | ready | 3 | 0.109095 | 1 |
| 1024 | 1 | uniform | ordered | 3 | 0.111258 | 1 |
| 1024 | 1 | uniform | ready | 8 | 0.111866 | 1 |
| 1024 | 1 | uniform | ordered | 8 | 0.107169 | 1 |
| 1024 | 1 | uniform | ready | 2 | 0.114165 | 1 |
| 1024 | 1 | uniform | ordered | 4 | 0.125895 | 1 |
| 1024 | 1 | uniform | ordered | 2 | 0.114122 | 1 |
| 1024 | 1 | uniform | ready | 7 | 0.116019 | 1 |
| 1024 | 1 | uniform | ordered | 0 | 0.116542 | 1 |
| 1024 | 1 | uniform | ordered | 5 | 0.112089 | 1 |
| 1024 | 1 | uniform | ordered | 1 | 0.113768 | 1 |
| 1024 | 1 | uniform | ordered | 7 | 0.121756 | 1 |
| 1024 | 1 | uniform | ready | 6 | 0.116365 | 1 |
| 1024 | 1 | uniform | ready | 0 | 0.115187 | 1 |
