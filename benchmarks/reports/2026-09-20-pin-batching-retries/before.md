# Small-blob pin protection

Complete: True

Fresh local payload store (loose and packed) and durable pin ledger. Timings cover staging only; publication, readback, duplicate writes and release checks are outside timing.
Ledger edits are revision advances, not syscall counts. Raw process output and binary identity are retained in JSON.

| Layout | Files | Bytes/file | Repetition | Seconds | Ledger edits |
|---|---:|---:|---:|---:|---:|
| packed | 16 | 64 | 1 | 0.037554 | 16 |
| packed | 16 | 511 | 1 | 0.030169 | 16 |
| packed | 16 | 512 | 1 | 0.048151 | 32 |
| packed | 16 | 513 | 1 | 0.167266 | 32 |
| loose | 16 | 64 | 1 | 0.111013 | 32 |
| loose | 16 | 511 | 1 | 0.141525 | 32 |
| loose | 16 | 512 | 1 | 0.080237 | 48 |
| loose | 16 | 513 | 1 | 0.067582 | 48 |
