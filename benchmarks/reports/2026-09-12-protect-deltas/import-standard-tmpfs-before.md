# Git import profile

Complete: True

Reference fd2f363 plus cached checkpoint index reuse; tmpfs destination; shared host; one repetition.

| Shape | Operation | Objects | Wall, s | Header+decode, s | Stage poll+drain, s | Checkpoints, s | RSS at import end, MiB |
|---|---|---:|---:|---:|---:|---:|---:|
| delta-heavy | initial-import | 14060 | 64.570 | 0.997 | 62.235 | 0.684 | 707.2 |
| delta-heavy | unchanged-import | 14060 | 0.020 | 0.000 | 0.000 | 0.000 | 59.8 |
| delta-heavy | incremental-import | 14200 | 0.332 | 0.012 | 0.207 | 0.027 | 153.5 |
