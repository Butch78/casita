# Git import profile

Complete: True

Candidate fd2f363 plus cached checkpoint index reuse; RAM-backed destination; shared host; one repetition.

| Shape | Operation | Objects | Wall, s | Header+decode, s | Stage poll+drain, s | Checkpoints, s | RSS at import end, MiB |
|---|---|---:|---:|---:|---:|---:|---:|
| delta-heavy | initial-import | 14060 | 60.784 | 0.993 | 58.263 | 0.810 | 709.6 |
| delta-heavy | unchanged-import | 14060 | 0.023 | 0.000 | 0.000 | 0.000 | 52.4 |
| delta-heavy | incremental-import | 14200 | 0.342 | 0.012 | 0.186 | 0.040 | 186.5 |
