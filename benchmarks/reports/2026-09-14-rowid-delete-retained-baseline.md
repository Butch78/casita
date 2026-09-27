# Metadata collection

Leaf-only metadata; every object retained. Setup and reopened inventory audit are outside commit timing.
First means the first collection after seeding, not a cold OS cache. Warm values are means per process; raw iterations are retained.
Process RSS includes setup and audit. These are local measurements, not a controlled revision comparison.

Complete: True

| Objects | Phase | Repetition | Mean operation ms |
|---:|---|---:|---:|
| 65537 | collect-first | 1 | 809.139 |
| 65537 | collect-warm | 1 | 896.633 |
| 65536 | collect-first | 1 | 348.205 |
| 65536 | collect-warm | 1 | 375.283 |
| 65536 | collect-first | 3 | 357.349 |
| 65536 | collect-warm | 3 | 309.562 |
| 65536 | collect-first | 2 | 291.160 |
| 65536 | collect-warm | 2 | 332.619 |
| 65537 | collect-first | 3 | 743.046 |
| 65537 | collect-warm | 3 | 851.031 |
| 65537 | collect-first | 2 | 778.564 |
| 65537 | collect-warm | 2 | 898.235 |
