# Local ledger boundaries

Complete: True

Every profile covers both sides of each limit. Setup and cold replay checks are outside timing.
Groups invoke the production batch executor directly, excluding asynchronous queue scheduling.
Migration space is simulated allocation denial, not a physically full device.
Byte cases add 256 KiB catalog records; the fourth frame crosses the 1 MiB journal window.

| Boundary | Position | Repetition | ms |
|---|---:|---:|---:|
| checkpoint-bytes | 3 | 1 | 4.019 |
| group-size | 64 | 3 | 3.964 |
| checkpoint-operations | 256 | 1 | 3.602 |
| group-size | 63 | 1 | 4.502 |
| checkpoint-operations | 257 | 3 | 7.447 |
| migration-space | 1 | 2 | 29.266 |
| migration-space | 1 | 1 | 28.912 |
| group-size | 65 | 3 | 8.645 |
| checkpoint-operations | 257 | 2 | 12.391 |
| checkpoint-operations | 256 | 3 | 3.622 |
| checkpoint-bytes | 4 | 3 | 9.644 |
| checkpoint-bytes | 3 | 3 | 4.151 |
| group-size | 64 | 2 | 4.353 |
| checkpoint-operations | 255 | 1 | 3.555 |
| migration-space | 0 | 1 | 26.285 |
| group-size | 2 | 2 | 3.704 |
| checkpoint-bytes | 3 | 2 | 4.299 |
| checkpoint-operations | 257 | 1 | 6.923 |
| group-size | 63 | 2 | 4.182 |
| checkpoint-operations | 255 | 2 | 3.622 |
| group-size | 65 | 2 | 10.883 |
| migration-space | 0 | 2 | 26.418 |
| checkpoint-bytes | 4 | 1 | 9.198 |
| checkpoint-bytes | 4 | 2 | 9.603 |
| group-size | 1 | 1 | 3.542 |
| migration-space | 0 | 3 | 30.022 |
| group-size | 1 | 2 | 3.716 |
| group-size | 2 | 1 | 3.587 |
| checkpoint-operations | 256 | 2 | 3.802 |
| group-size | 63 | 3 | 4.122 |
| checkpoint-bytes | 5 | 1 | 4.144 |
| group-size | 1 | 3 | 3.671 |
| group-size | 65 | 1 | 9.586 |
| migration-space | 1 | 3 | 27.886 |
| group-size | 2 | 3 | 3.700 |
| group-size | 64 | 1 | 4.013 |
| checkpoint-bytes | 5 | 2 | 3.987 |
| checkpoint-bytes | 5 | 3 | 5.349 |
| checkpoint-operations | 255 | 3 | 3.556 |
