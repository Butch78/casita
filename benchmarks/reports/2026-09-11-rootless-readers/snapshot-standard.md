# Snapshot connections

Complete: True

acquire/query/release burst; fresh control also destroys idle connections; writer commits excluded

Shared development host; unrelated builds may run. No overlapping builds or tests launched by this investigation. Fresh control clears the candidate idle pool; not a historical binary comparison.

| Live readers | Mode | Repetition | microseconds/snapshot |
|---:|---|---:|---:|
| 9 | fresh | 3 | 204.64 |
| 7 | reused | 3 | 165.56 |
| 1 | fresh | 2 | 773.41 |
| 1 | fresh | 1 | 767.73 |
| 8 | fresh | 3 | 201.41 |
| 1 | reused | 1 | 669.61 |
| 1 | fresh | 3 | 747.69 |
| 9 | fresh | 1 | 78.17 |
| 1 | reused | 2 | 215.31 |
| 7 | fresh | 2 | 66.29 |
| 7 | fresh | 1 | 66.47 |
| 16 | fresh | 1 | 59.49 |
| 7 | reused | 1 | 52.64 |
| 9 | reused | 2 | 47.10 |
| 8 | reused | 3 | 44.43 |
| 9 | reused | 1 | 34.38 |
| 8 | fresh | 2 | 59.95 |
| 16 | fresh | 2 | 48.52 |
| 16 | reused | 2 | 31.67 |
| 8 | reused | 1 | 32.04 |
| 16 | reused | 3 | 31.07 |
| 9 | fresh | 2 | 56.77 |
| 8 | fresh | 1 | 57.68 |
| 1 | reused | 3 | 158.16 |
| 9 | reused | 3 | 29.57 |
| 16 | reused | 1 | 30.51 |
| 8 | reused | 2 | 30.62 |
| 7 | fresh | 3 | 63.47 |
| 7 | reused | 2 | 45.34 |
| 16 | fresh | 3 | 129.04 |
