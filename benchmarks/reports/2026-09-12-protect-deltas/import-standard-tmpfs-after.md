# Git import profile

Complete: True

Candidate resource-addition journal frames on top of cached checkpoint index reuse; tmpfs destination; shared host; one repetition.

| Shape | Operation | Objects | Wall, s | Header+decode, s | Stage poll+drain, s | Checkpoints, s | RSS at import end, MiB |
|---|---|---:|---:|---:|---:|---:|---:|
| delta-heavy | initial-import | 14060 | 8.476 | 0.971 | 6.804 | 0.391 | 424.0 |
| delta-heavy | unchanged-import | 14060 | 0.026 | 0.000 | 0.000 | 0.000 | 51.1 |
| delta-heavy | incremental-import | 14200 | 0.229 | 0.016 | 0.101 | 0.022 | 127.4 |
