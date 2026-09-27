# Local ledger boundaries

Complete: True

Every profile covers both sides of each limit. Setup and cold replay checks are outside timing.
Groups invoke the production batch executor directly, excluding asynchronous queue scheduling.
Migration space is simulated allocation denial, not a physically full device.
Byte cases add 256 KiB catalog records; the fourth frame crosses the 1 MiB journal window.

Record-byte cases time three tiny protection edits on one retained catalog near 1 MiB; V2 frames encode only additions.

| Boundary | Position | Repetition | ms |
|---|---:|---:|---:|
| group-size | 1 | 1 | 55.967 |
| group-size | 64 | 1 | 16.682 |
| checkpoint-operations | 255 | 1 | 8.210 |
| checkpoint-bytes | 3 | 1 | 9.967 |
| checkpoint-bytes | 5 | 1 | 36.704 |
| migration-space | 1 | 1 | 112.549 |
| checkpoint-operations | 256 | 1 | 7.218 |
| checkpoint-record-bytes | 1044480 | 1 | 83.761 |
| checkpoint-record-bytes | 1052672 | 1 | 80.867 |
| checkpoint-operations | 257 | 1 | 14.256 |
| group-size | 2 | 1 | 35.766 |
| migration-space | 0 | 1 | 98.135 |
| checkpoint-bytes | 4 | 1 | 14.010 |
| checkpoint-record-bytes | 1048576 | 1 | 103.263 |
| group-size | 63 | 1 | 20.298 |
| group-size | 65 | 1 | 28.489 |
