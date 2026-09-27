# Catalog WAL footprint

Complete: True

Real catalog encodings and Turso metadata commits; payload objects are not materialized.
Reference mode models SQL storage only. Timing excludes fixture construction and audits.
WAL bytes are file length, not cumulative write traffic. PASSIVE checkpoint runs after measurement.

| Case | Mode | Held reader | Rep | Catalog bytes | SQL bytes | WAL bytes | Checkpoint (busy/log/done) |
|---|---|---|---:|---:|---:|---:|---|
| small | resubmit | False | 1 | 275 | 275 | 131872 | [0, 32, 32] |
| small | resubmit | True | 1 | 275 | 275 | 131872 | [1, None, None] |
| small | metadata-only | False | 1 | 275 | 275 | 131872 | [0, 32, 32] |
| small | metadata-only | True | 1 | 275 | 275 | 131872 | [1, None, None] |
| small | reference | False | 1 | 275 | 48 | 131872 | [0, 32, 32] |
| small | reference | True | 1 | 275 | 48 | 131872 | [1, None, None] |
| delta-below | resubmit | False | 1 | 1048307 | 1048307 | 4268352 | [0, 1036, 1036] |
| delta-below | resubmit | True | 1 | 1048307 | 1048307 | 34146592 | [1, None, None] |
| delta-below | metadata-only | False | 1 | 1048307 | 1048307 | 4268352 | [0, 1036, 1036] |
| delta-below | metadata-only | True | 1 | 1048307 | 1048307 | 34146592 | [1, None, None] |
| delta-below | reference | False | 1 | 1048307 | 48 | 131872 | [0, 32, 32] |
| delta-below | reference | True | 1 | 1048307 | 48 | 131872 | [1, None, None] |
| delta-above | resubmit | False | 1 | 276 | 276 | 131872 | [0, 32, 32] |
| delta-above | resubmit | True | 1 | 276 | 276 | 131872 | [1, None, None] |
| delta-above | metadata-only | False | 1 | 276 | 276 | 131872 | [0, 32, 32] |
| delta-above | metadata-only | True | 1 | 276 | 276 | 131872 | [1, None, None] |
| delta-above | reference | False | 1 | 276 | 48 | 131872 | [0, 32, 32] |
| delta-above | reference | True | 1 | 276 | 48 | 131872 | [1, None, None] |
| base-below | resubmit | False | 1 | 4194346 | 4194346 | 4239512 | [0, 1029, 1029] |
| base-below | resubmit | True | 1 | 4194346 | 4194346 | 135663392 | [1, None, None] |
| base-below | metadata-only | False | 1 | 4194346 | 4194346 | 4239512 | [0, 1029, 1029] |
| base-below | metadata-only | True | 1 | 4194346 | 4194346 | 135663392 | [1, None, None] |
| base-below | reference | False | 1 | 4194346 | 48 | 131872 | [0, 32, 32] |
| base-below | reference | True | 1 | 4194346 | 48 | 131872 | [1, None, None] |
| base-above | resubmit | False | 1 | 98 | 98 | 131872 | [0, 32, 32] |
| base-above | resubmit | True | 1 | 98 | 98 | 131872 | [1, None, None] |
| base-above | metadata-only | False | 1 | 98 | 98 | 131872 | [0, 32, 32] |
| base-above | metadata-only | True | 1 | 98 | 98 | 131872 | [1, None, None] |
| base-above | reference | False | 1 | 98 | 48 | 131872 | [0, 32, 32] |
| base-above | reference | True | 1 | 98 | 48 | 131872 | [1, None, None] |
| small | resubmit | False | 2 | 275 | 275 | 131872 | [0, 32, 32] |
| small | resubmit | True | 2 | 275 | 275 | 131872 | [1, None, None] |
| small | metadata-only | False | 2 | 275 | 275 | 131872 | [0, 32, 32] |
| small | metadata-only | True | 2 | 275 | 275 | 131872 | [1, None, None] |
| small | reference | False | 2 | 275 | 48 | 131872 | [0, 32, 32] |
| small | reference | True | 2 | 275 | 48 | 131872 | [1, None, None] |
| delta-below | resubmit | False | 2 | 1048307 | 1048307 | 4268352 | [0, 1036, 1036] |
| delta-below | resubmit | True | 2 | 1048307 | 1048307 | 34146592 | [1, None, None] |
| delta-below | metadata-only | False | 2 | 1048307 | 1048307 | 4268352 | [0, 1036, 1036] |
| delta-below | metadata-only | True | 2 | 1048307 | 1048307 | 34146592 | [1, None, None] |
| delta-below | reference | False | 2 | 1048307 | 48 | 131872 | [0, 32, 32] |
| delta-below | reference | True | 2 | 1048307 | 48 | 131872 | [1, None, None] |
| delta-above | resubmit | False | 2 | 276 | 276 | 131872 | [0, 32, 32] |
| delta-above | resubmit | True | 2 | 276 | 276 | 131872 | [1, None, None] |
| delta-above | metadata-only | False | 2 | 276 | 276 | 131872 | [0, 32, 32] |
| delta-above | metadata-only | True | 2 | 276 | 276 | 131872 | [1, None, None] |
| delta-above | reference | False | 2 | 276 | 48 | 131872 | [0, 32, 32] |
| delta-above | reference | True | 2 | 276 | 48 | 131872 | [1, None, None] |
| base-below | resubmit | False | 2 | 4194346 | 4194346 | 4239512 | [0, 1029, 1029] |
| base-below | resubmit | True | 2 | 4194346 | 4194346 | 135663392 | [1, None, None] |
| base-below | metadata-only | False | 2 | 4194346 | 4194346 | 4239512 | [0, 1029, 1029] |
| base-below | metadata-only | True | 2 | 4194346 | 4194346 | 135663392 | [1, None, None] |
| base-below | reference | False | 2 | 4194346 | 48 | 131872 | [0, 32, 32] |
| base-below | reference | True | 2 | 4194346 | 48 | 131872 | [1, None, None] |
| base-above | resubmit | False | 2 | 98 | 98 | 131872 | [0, 32, 32] |
| base-above | resubmit | True | 2 | 98 | 98 | 131872 | [1, None, None] |
| base-above | metadata-only | False | 2 | 98 | 98 | 131872 | [0, 32, 32] |
| base-above | metadata-only | True | 2 | 98 | 98 | 131872 | [1, None, None] |
| base-above | reference | False | 2 | 98 | 48 | 131872 | [0, 32, 32] |
| base-above | reference | True | 2 | 98 | 48 | 131872 | [1, None, None] |
| small | resubmit | False | 3 | 275 | 275 | 131872 | [0, 32, 32] |
| small | resubmit | True | 3 | 275 | 275 | 131872 | [1, None, None] |
| small | metadata-only | False | 3 | 275 | 275 | 131872 | [0, 32, 32] |
| small | metadata-only | True | 3 | 275 | 275 | 131872 | [1, None, None] |
| small | reference | False | 3 | 275 | 48 | 131872 | [0, 32, 32] |
| small | reference | True | 3 | 275 | 48 | 131872 | [1, None, None] |
| delta-below | resubmit | False | 3 | 1048307 | 1048307 | 4268352 | [0, 1036, 1036] |
| delta-below | resubmit | True | 3 | 1048307 | 1048307 | 34146592 | [1, None, None] |
| delta-below | metadata-only | False | 3 | 1048307 | 1048307 | 4268352 | [0, 1036, 1036] |
| delta-below | metadata-only | True | 3 | 1048307 | 1048307 | 34146592 | [1, None, None] |
| delta-below | reference | False | 3 | 1048307 | 48 | 131872 | [0, 32, 32] |
| delta-below | reference | True | 3 | 1048307 | 48 | 131872 | [1, None, None] |
| delta-above | resubmit | False | 3 | 276 | 276 | 131872 | [0, 32, 32] |
| delta-above | resubmit | True | 3 | 276 | 276 | 131872 | [1, None, None] |
| delta-above | metadata-only | False | 3 | 276 | 276 | 131872 | [0, 32, 32] |
| delta-above | metadata-only | True | 3 | 276 | 276 | 131872 | [1, None, None] |
| delta-above | reference | False | 3 | 276 | 48 | 131872 | [0, 32, 32] |
| delta-above | reference | True | 3 | 276 | 48 | 131872 | [1, None, None] |
| base-below | resubmit | False | 3 | 4194346 | 4194346 | 4239512 | [0, 1029, 1029] |
| base-below | resubmit | True | 3 | 4194346 | 4194346 | 135663392 | [1, None, None] |
| base-below | metadata-only | False | 3 | 4194346 | 4194346 | 4239512 | [0, 1029, 1029] |
| base-below | metadata-only | True | 3 | 4194346 | 4194346 | 135663392 | [1, None, None] |
| base-below | reference | False | 3 | 4194346 | 48 | 131872 | [0, 32, 32] |
| base-below | reference | True | 3 | 4194346 | 48 | 131872 | [1, None, None] |
| base-above | resubmit | False | 3 | 98 | 98 | 131872 | [0, 32, 32] |
| base-above | resubmit | True | 3 | 98 | 98 | 131872 | [1, None, None] |
| base-above | metadata-only | False | 3 | 98 | 98 | 131872 | [0, 32, 32] |
| base-above | metadata-only | True | 3 | 98 | 98 | 131872 | [1, None, None] |
| base-above | reference | False | 3 | 98 | 48 | 131872 | [0, 32, 32] |
| base-above | reference | True | 3 | 98 | 48 | 131872 | [1, None, None] |
