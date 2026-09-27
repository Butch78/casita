# Local reader coordination

Complete: True

Cold measures first owner registration. Warm phases run with the specified other readers active.
Boundary setup moves the real counter to its last reserved revision outside timing; rollover uses the production reserve path.
Timing includes the blocking worker and atomic inventory replacement. Durability gates compare the ledger bytes outside timing.
Each warm value is the mean per process. Raw iterations are retained. OS caches are not flushed.

| Other readers | Phase | Repetition | Mean ms | Durable ledger changed |
|---:|---|---:|---:|---:|
| 1 | cold-register | 1 | 26.612 | 1 |
| 1 | warm-register | 1 | 0.139 | 0 |
| 1 | warm-protect | 1 | 0.136 | 0 |
| 1 | warm-release | 1 | 0.131 | 0 |
| 1 | last-reserved-register | 1 | 0.451 | 0 |
| 1 | reservation-rollover-protect | 1 | 19.011 | 1 |
| 1 | renewed-release | 1 | 0.274 | 0 |
| 64 | cold-register | 1 | 19.155 | 1 |
| 64 | warm-register | 1 | 0.141 | 0 |
| 64 | warm-protect | 1 | 0.140 | 0 |
| 64 | warm-release | 1 | 0.142 | 0 |
| 64 | last-reserved-register | 1 | 0.389 | 0 |
| 64 | reservation-rollover-protect | 1 | 14.837 | 1 |
| 64 | renewed-release | 1 | 0.176 | 0 |
| 1 | cold-register | 2 | 18.952 | 1 |
| 1 | warm-register | 2 | 0.141 | 0 |
| 1 | warm-protect | 2 | 0.142 | 0 |
| 1 | warm-release | 2 | 0.140 | 0 |
| 1 | last-reserved-register | 2 | 0.278 | 0 |
| 1 | reservation-rollover-protect | 2 | 14.803 | 1 |
| 1 | renewed-release | 2 | 0.254 | 0 |
| 64 | cold-register | 2 | 19.331 | 1 |
| 64 | warm-register | 2 | 0.156 | 0 |
| 64 | warm-protect | 2 | 0.157 | 0 |
| 64 | warm-release | 2 | 0.162 | 0 |
| 64 | last-reserved-register | 2 | 0.278 | 0 |
| 64 | reservation-rollover-protect | 2 | 15.408 | 1 |
| 64 | renewed-release | 2 | 0.332 | 0 |
| 1 | cold-register | 3 | 18.688 | 1 |
| 1 | warm-register | 3 | 0.157 | 0 |
| 1 | warm-protect | 3 | 0.160 | 0 |
| 1 | warm-release | 3 | 0.135 | 0 |
| 1 | last-reserved-register | 3 | 0.322 | 0 |
| 1 | reservation-rollover-protect | 3 | 14.954 | 1 |
| 1 | renewed-release | 3 | 0.259 | 0 |
| 64 | cold-register | 3 | 18.579 | 1 |
| 64 | warm-register | 3 | 0.154 | 0 |
| 64 | warm-protect | 3 | 0.149 | 0 |
| 64 | warm-release | 3 | 0.151 | 0 |
| 64 | last-reserved-register | 3 | 0.584 | 0 |
| 64 | reservation-rollover-protect | 3 | 14.801 | 1 |
| 64 | renewed-release | 3 | 0.243 | 0 |
