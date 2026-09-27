# Metadata collection

Leaf-only metadata; every object retained. Setup and reopened inventory audit are outside commit timing.
First means the first collection after seeding, not a cold OS cache. Warm values are means per process; raw iterations are retained.
Process RSS includes setup and audit. These are local measurements, not a controlled revision comparison.

Complete: True

| Objects | Phase | Repetition | Mean operation ms |
|---:|---|---:|---:|
| 65536 | collect-first | 1 | 269.590 |
| 65536 | collect-warm | 1 | 242.808 |
| 65537 | collect-first | 1 | 706.822 |
| 65537 | collect-warm | 1 | 822.857 |
