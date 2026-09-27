# Local ledger boundaries

Complete: True

Every profile covers both sides of each limit. Setup and cold replay checks are outside timing.
Groups invoke the production batch executor directly, excluding asynchronous queue scheduling.
Migration space is simulated allocation denial, not a physically full device.
Byte cases add 256 KiB catalog records; the fourth frame crosses the 1 MiB journal window.

| Boundary | Position | Repetition | ms |
|---|---:|---:|---:|
| checkpoint-bytes | 3 | 1 | 4.277 |
| group-size | 64 | 3 | 7.822 |
| checkpoint-operations | 256 | 1 | 5.620 |
| group-size | 63 | 1 | 7.904 |
| checkpoint-operations | 257 | 3 | 10.151 |
| migration-space | 1 | 2 | 38.918 |
| migration-space | 1 | 1 | 46.742 |
| group-size | 65 | 3 | 13.861 |
| checkpoint-operations | 257 | 2 | 11.963 |
| checkpoint-operations | 256 | 3 | 6.693 |
| checkpoint-bytes | 4 | 3 | 19.139 |
| checkpoint-bytes | 3 | 3 | 6.367 |
| group-size | 64 | 2 | 10.460 |
| checkpoint-operations | 255 | 1 | 6.600 |
| migration-space | 0 | 1 | 29.702 |
| group-size | 2 | 2 | 4.707 |
| checkpoint-bytes | 3 | 2 | 5.707 |
| checkpoint-operations | 257 | 1 | 11.787 |
| group-size | 63 | 2 | 7.886 |
| checkpoint-operations | 255 | 2 | 4.735 |
| group-size | 65 | 2 | 14.786 |
| migration-space | 0 | 2 | 32.789 |
| checkpoint-bytes | 4 | 1 | 18.290 |
| checkpoint-bytes | 4 | 2 | 15.929 |
| group-size | 1 | 1 | 5.457 |
| migration-space | 0 | 3 | 29.821 |
| group-size | 1 | 2 | 3.001 |
| group-size | 2 | 1 | 5.405 |
| checkpoint-operations | 256 | 2 | 5.522 |
| group-size | 63 | 3 | 9.676 |
| checkpoint-bytes | 5 | 1 | 6.026 |
| group-size | 1 | 3 | 5.555 |
| group-size | 65 | 1 | 13.477 |
| migration-space | 1 | 3 | 45.718 |
| group-size | 2 | 3 | 3.652 |
| group-size | 64 | 1 | 8.738 |
| checkpoint-bytes | 5 | 2 | 5.955 |
| checkpoint-bytes | 5 | 3 | 5.062 |
| checkpoint-operations | 255 | 3 | 3.320 |
