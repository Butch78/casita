# Local ledger boundaries

Complete: True

Every profile covers both sides of each limit. Setup and cold replay checks are outside timing.
Groups invoke the production batch executor directly, excluding asynchronous queue scheduling.
Migration space is simulated allocation denial, not a physically full device.
Byte cases add 256 KiB catalog records; the fourth frame crosses the 1 MiB journal window.

Record-byte cases time three tiny protection edits on one retained catalog near 1 MiB; every edit re-encodes that record.

| Boundary | Position | Repetition | ms |
|---|---:|---:|---:|
| checkpoint-record-bytes | 1052672 | 2 | 41.510 |
| migration-space | 1 | 1 | 85.275 |
| checkpoint-operations | 255 | 1 | 4.447 |
| group-size | 63 | 2 | 7.687 |
| migration-space | 0 | 2 | 23.659 |
| checkpoint-record-bytes | 1048576 | 1 | 41.674 |
| checkpoint-operations | 257 | 3 | 7.921 |
| checkpoint-record-bytes | 1048576 | 2 | 62.085 |
| checkpoint-record-bytes | 1044480 | 3 | 39.091 |
| checkpoint-record-bytes | 1052672 | 3 | 51.302 |
| checkpoint-bytes | 4 | 2 | 18.750 |
| checkpoint-operations | 255 | 3 | 4.580 |
| checkpoint-bytes | 4 | 3 | 12.752 |
| checkpoint-bytes | 4 | 1 | 12.706 |
| migration-space | 0 | 3 | 25.588 |
| checkpoint-bytes | 3 | 1 | 8.651 |
| group-size | 65 | 3 | 12.441 |
| checkpoint-bytes | 5 | 1 | 5.377 |
| checkpoint-bytes | 3 | 3 | 5.523 |
| checkpoint-bytes | 5 | 3 | 12.145 |
| group-size | 64 | 2 | 6.855 |
| checkpoint-record-bytes | 1052672 | 1 | 58.346 |
| checkpoint-operations | 256 | 1 | 4.695 |
| group-size | 1 | 2 | 3.662 |
| checkpoint-operations | 255 | 2 | 3.679 |
| checkpoint-operations | 257 | 1 | 9.708 |
| checkpoint-bytes | 3 | 2 | 4.745 |
| checkpoint-record-bytes | 1044480 | 1 | 29.484 |
| checkpoint-record-bytes | 1044480 | 2 | 44.358 |
| group-size | 65 | 1 | 11.681 |
| checkpoint-operations | 256 | 2 | 7.649 |
| group-size | 1 | 3 | 4.727 |
| group-size | 2 | 3 | 4.255 |
| group-size | 64 | 1 | 8.637 |
| checkpoint-bytes | 5 | 2 | 4.385 |
| migration-space | 0 | 1 | 25.533 |
| group-size | 65 | 2 | 13.700 |
| group-size | 63 | 1 | 7.707 |
| checkpoint-record-bytes | 1048576 | 3 | 43.479 |
| group-size | 2 | 1 | 7.943 |
| migration-space | 1 | 3 | 39.623 |
| checkpoint-operations | 256 | 3 | 4.621 |
| group-size | 1 | 1 | 4.446 |
| group-size | 63 | 3 | 7.784 |
| checkpoint-operations | 257 | 2 | 9.945 |
| migration-space | 1 | 2 | 40.886 |
| group-size | 64 | 3 | 7.612 |
| group-size | 2 | 2 | 3.674 |
