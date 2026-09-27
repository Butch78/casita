# Metadata collection

Leaf-only metadata; every object retained. Setup and reopened inventory audit are outside commit timing.
First means the first collection after seeding, not a cold OS cache. Warm values are means per process; raw iterations are retained.
Process RSS includes setup and audit. These are local measurements, not a controlled revision comparison.

Complete: True

| Objects | Phase | Repetition | Mean operation ms |
|---:|---|---:|---:|
| 65536 | collect-first | 1 | 257.710 |
| 65536 | collect-warm | 1 | 238.310 |
| 65537 | collect-first | 1 | 687.773 |
| 65537 | collect-warm | 1 | 818.898 |
