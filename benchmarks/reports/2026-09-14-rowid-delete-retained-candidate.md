# Metadata collection

Leaf-only metadata; every object retained. Setup and reopened inventory audit are outside commit timing.
First means the first collection after seeding, not a cold OS cache. Warm values are means per process; raw iterations are retained.
Process RSS includes setup and audit. These are local measurements, not a controlled revision comparison.

Complete: True

| Objects | Phase | Repetition | Mean operation ms |
|---:|---|---:|---:|
| 65537 | collect-first | 1 | 807.764 |
| 65537 | collect-warm | 1 | 899.806 |
| 65536 | collect-first | 1 | 285.407 |
| 65536 | collect-warm | 1 | 285.694 |
| 65536 | collect-first | 3 | 320.715 |
| 65536 | collect-warm | 3 | 293.301 |
| 65536 | collect-first | 2 | 347.423 |
| 65536 | collect-warm | 2 | 321.563 |
| 65537 | collect-first | 3 | 791.025 |
| 65537 | collect-warm | 3 | 945.055 |
| 65537 | collect-first | 2 | 805.618 |
| 65537 | collect-warm | 2 | 898.894 |
