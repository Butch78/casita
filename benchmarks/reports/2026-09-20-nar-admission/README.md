# NAR admission batching, 2026-09-20

Baseline: `002b13f`. The final candidate is the commit containing this report.
The same permanent 256-file, 1,024-byte raw NAR regression exercises a fresh
local repository with a durable ledger, exact canonical hash validation, and
an independent stored-content scrub. Measurements count ledger journal syncs;
these debug binaries do not establish release-throughput changes.

| Variant | Journal syncs per import |
| --- | --- |
| Baseline, five initial samples | 41, 41, 41, 41, 41 |
| Yield before admission, five samples | 43, 46, 43, 45, 46 |
| Buffered small-file delivery, five samples | 41, 41, 41, 41, 41 |
| Baseline, ready-window ABBA samples | 41, 40, 41, 41 |
| Ready window, before allocation fix | 25, 25, 25, 25 |
| Baseline, final ABBA samples | 47, 48, 55, 49 |
| Final candidate, including allocation fix | 25, 25, 25, 26 |

The ready-window comparison reduces the median from 41 to 25, about 39%.
After the allocation fix, a fresh alternating run measured 48.5 versus 25.
Scheduling changes group sizes, so all observations are retained instead of
claiming a fixed percentage for every import. The final version expands the
bounded event channel to 16 slots, drains only immediately ready events into
the existing 16-event staging window, and yields once after a missing pin
request is queued. That lets sibling requests join the first durable edit.
Confirmed identities still take the synchronous cache path.

The yield alone was ineffective because sibling stages were not ready yet.
Buffering files up to 64 KiB also failed to reduce syncs. The final implementation
uses the existing pipes; the added whole-file buffer type is discarded.
`yield-only.patch`, `buffered-delivery.patch`, and `buffered-yield.patch` retain
the exploratory variants. Raw output and binary identities are in the JSON files.

At most 16 active events, 16 queued events, and one decoder event hold delivery
pipes. Their aggregate capacity is at most 33 * 64 KiB (2.0625 MiB), excluding
the unchanged wire pipe and blob writer working sets. The maximum added capacity
relative to the original one-slot queue is 960 KiB. Large files remain streamed.
The consumer never waits for the queue to fill, and it drains active stages
before directory publication can acquire their pin gate.

Reproduce the measured correctness regression using either preserved binary:

```sh
BINARY nar::tests::concurrent_raw_intake_groups_durable_pin_admissions --exact --nocapture
```

Or build it and the permanent Criterion corpus in the development shell:

```sh
cargo test --lib nar::tests::concurrent_raw_intake_groups_durable_pin_admissions -- --exact --nocapture
cargo bench --bench nar_import
cargo test --bench nar_import -- --test
```

The registered `nar_import` corpus, included in `benchmark all`, now includes
the measured 256-file case and 512/2,048 files of 65,536 bytes. Existing cases
cover 15/16/17 events and 65,535/65,536/65,537-byte files across the pipe boundary.
Cancellation tests cover both sides of that boundary; the paused-source test
requires publication of a completed file without waiting for more input.

## Compressed staging allocations

The new heap regression initially failed: importing 512 unique 64 KiB files
(32 MiB total) grew the tracked Rust heap by 39,145,317 bytes. Zstd's bulk encoder
reserves its worst-case output capacity, and converting its short compressed
result to `Bytes` retained that capacity in pack staging. Pack limits account
for encoded length, so highly compressible files retained plaintext-sized
allocations despite a small encoded batch.

`compression::compress` now shrinks the finished vector before returning it.
Encoded contents and on-disk formats are unchanged. The regression covers both
compressible and incompressible frames, including repeated codec-context reuse.
The same integration test then measured 8,122,485 bytes for 512 files and
11,164,867 bytes for 2,048 files (128 MiB total), both below its unchanged 24 MiB
ceiling. It generates the archive on disk using one reusable payload buffer.
The counter measures Rust allocations, not RSS or allocations internal to C
libraries. `heap-results.json` retains the failed and successful observations.

## Validation

The batching changes passed 705 library tests (40 ignored), including the full
process-death matrices. After the capacity fix, all three codec tests, 74
chunked-store tests, 37 NAR tests, all 31 permanent NAR benchmark correctness
cases, formatting, and all-features/all-targets Clippy passed. The expanded
Obrador heap test passed after the fix. Full Obrador core and compatibility
tests also passed before the capacity-only adjustment.
