# Small-blob pin protection

Complete: True

Fresh local payload store (loose and packed) and durable pin ledger. Timings cover staging only; publication, readback, duplicate writes and release checks are outside timing.
Ledger edits are revision advances, not syscall counts. Raw process output and binary identity are retained in JSON.

| Layout | Files | Bytes/file | Repetition | Seconds | Ledger edits |
|---|---:|---:|---:|---:|---:|
| packed | 16 | 64 | 1 | 0.098912 | 16 |
| packed | 16 | 511 | 1 | 0.195994 | 16 |
| packed | 16 | 512 | 1 | 0.082627 | 32 |
| packed | 16 | 513 | 1 | 0.246875 | 32 |
| loose | 16 | 64 | 1 | 0.070489 | 16 |
| loose | 16 | 511 | 1 | 0.109848 | 16 |
| loose | 16 | 512 | 1 | 0.142945 | 32 |
| loose | 16 | 513 | 1 | 0.101632 | 32 |
