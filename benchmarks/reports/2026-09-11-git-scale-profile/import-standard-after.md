# Git import profile

Complete: True

Candidate validation on the standard 2 GiB logical delta-heavy history; fixed defaults 16 objects and 64 MiB; shared Btrfs host above 80 percent storage usage; no own builds or fixture generation during measurements; no completed standard baseline for an end-to-end speedup claim.

| Shape | Operation | Objects | Wall, s | Header+decode, s | Stage poll+drain, s | Checkpoints, s | RSS at import end, MiB |
|---|---|---:|---:|---:|---:|---:|---:|
| delta-heavy | initial-import | 14060 | 310.786 | 1.150 | 303.210 | 1.625 | 1093.1 |
| delta-heavy | unchanged-import | 14060 | 0.747 | 0.000 | 0.000 | 0.000 | 55.6 |
| delta-heavy | incremental-import | 14200 | 3.586 | 0.019 | 2.626 | 0.127 | 171.3 |

A 10-second perf recording at 99 Hz was attached after the second initial-import checkpoint. Its profiling overhead is included.
