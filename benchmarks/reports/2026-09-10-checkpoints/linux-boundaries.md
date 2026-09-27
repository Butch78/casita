# Local ledger boundaries

Complete: True

Every profile covers both sides of each limit. Setup and cold replay checks are outside timing.
Groups invoke the production batch executor directly, excluding asynchronous queue scheduling.
Migration space is simulated allocation denial, not a physically full device.
Byte cases add 256 KiB catalog records; the fourth frame crosses the 1 MiB journal window.

Record-byte cases time three tiny protection edits on one retained catalog near 1 MiB; every edit re-encodes that record.

| Boundary | Position | Repetition | ms |
|---|---:|---:|---:|
| checkpoint-record-bytes | 1052672 | 2 | 38.612 |
| migration-space | 1 | 1 | 39.399 |
| checkpoint-operations | 255 | 1 | 1.242 |
| group-size | 63 | 2 | 8.274 |
| migration-space | 0 | 2 | 53.793 |
| checkpoint-record-bytes | 1048576 | 1 | 34.771 |
| checkpoint-operations | 257 | 3 | 2.360 |
| checkpoint-record-bytes | 1048576 | 2 | 42.110 |
| checkpoint-record-bytes | 1044480 | 3 | 42.730 |
| checkpoint-record-bytes | 1052672 | 3 | 45.888 |
| checkpoint-bytes | 4 | 2 | 5.649 |
| checkpoint-operations | 255 | 3 | 1.709 |
| checkpoint-bytes | 4 | 3 | 5.811 |
| checkpoint-bytes | 4 | 1 | 5.533 |
| migration-space | 0 | 3 | 35.371 |
| checkpoint-bytes | 3 | 1 | 2.297 |
| group-size | 65 | 3 | 12.058 |
| checkpoint-bytes | 5 | 1 | 15.637 |
| checkpoint-bytes | 3 | 3 | 2.214 |
| checkpoint-bytes | 5 | 3 | 15.286 |
| group-size | 64 | 2 | 9.000 |
| checkpoint-record-bytes | 1052672 | 1 | 41.199 |
| checkpoint-operations | 256 | 1 | 1.082 |
| group-size | 1 | 2 | 8.379 |
| checkpoint-operations | 255 | 2 | 1.064 |
| checkpoint-operations | 257 | 1 | 2.573 |
| checkpoint-bytes | 3 | 2 | 1.581 |
| checkpoint-record-bytes | 1044480 | 1 | 39.232 |
| checkpoint-record-bytes | 1044480 | 2 | 42.160 |
| group-size | 65 | 1 | 10.491 |
| checkpoint-operations | 256 | 2 | 1.819 |
| group-size | 1 | 3 | 8.711 |
| group-size | 2 | 3 | 8.317 |
| group-size | 64 | 1 | 9.482 |
| checkpoint-bytes | 5 | 2 | 15.422 |
| migration-space | 0 | 1 | 33.625 |
| group-size | 65 | 2 | 10.085 |
| group-size | 63 | 1 | 9.245 |
| checkpoint-record-bytes | 1048576 | 3 | 39.628 |
| group-size | 2 | 1 | 8.606 |
| migration-space | 1 | 3 | 36.564 |
| checkpoint-operations | 256 | 3 | 1.129 |
| group-size | 1 | 1 | 9.870 |
| group-size | 63 | 3 | 12.254 |
| checkpoint-operations | 257 | 2 | 2.301 |
| migration-space | 1 | 2 | 37.916 |
| group-size | 64 | 3 | 11.165 |
| group-size | 2 | 2 | 10.074 |
