# Snapshot connections

Complete: True

acquire/query/release burst; fresh control also destroys idle connections; writer commits excluded



| Live readers | Mode | Repetition | microseconds/snapshot |
|---:|---|---:|---:|
| 16 | fresh | 1 | 62.12 |
| 7 | fresh | 1 | 94.71 |
| 1 | fresh | 1 | 353.37 |
| 7 | reused | 1 | 53.22 |
| 1 | reused | 1 | 235.24 |
| 16 | reused | 1 | 46.55 |
| 8 | reused | 1 | 52.93 |
| 8 | fresh | 1 | 79.03 |
| 9 | reused | 1 | 45.07 |
| 9 | fresh | 1 | 75.54 |
