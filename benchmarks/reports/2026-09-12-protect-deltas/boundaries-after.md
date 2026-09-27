# Local ledger boundaries

Complete: True

Every profile covers both sides of each limit. Setup and cold replay checks are outside timing.
Groups invoke the production batch executor directly, excluding asynchronous queue scheduling.
Migration space is simulated allocation denial, not a physically full device.
Byte cases add 256 KiB catalog records; the fourth frame crosses the 1 MiB journal window.

Record-byte cases time three tiny protection edits on one retained catalog near 1 MiB; V2 frames encode only additions.

| Boundary | Position | Repetition | ms |
|---|---:|---:|---:|
| group-size | 1 | 1 | 19.310 |
| group-size | 64 | 1 | 30.394 |
| checkpoint-operations | 255 | 1 | 6.940 |
| checkpoint-bytes | 3 | 1 | 6.303 |
| checkpoint-bytes | 5 | 1 | 32.588 |
| migration-space | 1 | 1 | 101.671 |
| checkpoint-operations | 256 | 1 | 6.671 |
| checkpoint-record-bytes | 1044480 | 1 | 42.432 |
| checkpoint-record-bytes | 1052672 | 1 | 26.771 |
| checkpoint-operations | 257 | 1 | 11.700 |
| group-size | 2 | 1 | 17.089 |
| migration-space | 0 | 1 | 79.225 |
| checkpoint-bytes | 4 | 1 | 14.584 |
| checkpoint-record-bytes | 1048576 | 1 | 27.974 |
| group-size | 63 | 1 | 16.069 |
| group-size | 65 | 1 | 32.413 |
