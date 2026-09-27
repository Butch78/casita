# Git import profile

Complete: True

Reference fd2f363; RAM-backed destination; shared host; one repetition.

| Shape | Operation | Objects | Wall, s | Header+decode, s | Stage poll+drain, s | Checkpoints, s | RSS at import end, MiB |
|---|---|---:|---:|---:|---:|---:|---:|
| delta-heavy | initial-import | 14060 | 184.308 | 1.250 | 180.990 | 0.966 | 1113.4 |
| delta-heavy | unchanged-import | 14060 | 0.023 | 0.000 | 0.000 | 0.000 | 59.0 |
| delta-heavy | incremental-import | 14200 | 0.387 | 0.013 | 0.237 | 0.023 | 232.1 |
