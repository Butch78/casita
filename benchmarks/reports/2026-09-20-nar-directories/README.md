# Bounded directory admission batching

Baseline: Casita `f3ad3d831b4dc039790688d4a04a46425c036a58`. Candidate: the
commit containing this report. Both real-closure executables use unchanged
Obrador `56c1780` sources. Binary SHA-256 hashes and raw counts are retained in
`results.json`. Detailed traces are retained in Obrador's
`benchmarks/nar-directories-2026-09-20`.

The NAR consumer already drains each 16-event file-staging window before
processing its completed events. It now queues directory writes from those
completed events, polls them concurrently, drains every write, and publishes
results in the original post-order sequence. Publication still respects the
repository's object-count limit. All sibling writes finish before publication acquires the admission gate.

Queued directories have at most 256 KiB of canonical encoded bytes in total.
An oversized directory is staged alone, after draining earlier work. The
existing event window bounds the number of queued writes to 16. Every queue
is drained before requesting another event window, including when the source
pauses. The byte budget excludes open traversal frames, map overhead, writer
buffers, and the directory currently being completed. It is not a total heap
limit for arbitrary archives. Formats and verification rules are unchanged.

## Measurements

| Ledger syscall interval | Before | After |
| --- | ---: | ---: |
| Directory staging only | 252 | 46 |
| Blob staging only | 222 | 220 |
| Path registration, including nested stages | 109 | 109 |
| Outside those stages | 94 | 91 |
| Total during imports | 677 | 466 |
| All traced sync syscalls | 1,368 | 1,159 |

The six-path closure contains 1,035 files, 26 symlinks, and 40,486,910 payload
bytes. Each traced run imports into a fresh store, checks reopened canonical
NAR hashes, and performs five warm registrations. Stage labels are exclusive
temporal partitions, not causal ownership of shared edits. All per-import
partitions reconcile to their ledger syscall counts. Scheduling changes group
sizes; these single before/after traces establish an observed reduction, not
a fixed percentage. Traces ran alongside builds/tests on a shared host, so their
elapsed times are not throughput evidence.

The permanent 256-directory microcase recorded 56 journal syncs and independently
scrubbed the resulting NAR. Existing 32/128 MiB file-import regressions used
8,124,931 and 11,282,529 bytes of peak Rust heap growth, respectively, below the
unchanged 24 MiB limit. These measure Rust allocations, not RSS or C allocations.

## Permanent corpus and correctness gates

The existing `nar_import` target in `benchmarks/manifest.json`, included in
`benchmark all`, now contains 41 cases. Added cases cover 15/16/17 and 256 unique
sibling directories, nested depth 15/16/17, and three large sibling directories
with 63/64/65 long symlinks. The latter encode to 259,883/264,008/268,133 bytes,
bracketing the 262,144-byte budget. Every benchmark checks the canonical NAR
hash, size, payload accounting, and an independent stored-content scrub.

Regression tests also cover empty directories, publication limits 1/3/1,024,
Git hashes, publication while input pauses, collection during intake,
cancellation cleanup, truncated input, and recovery after a staging validation
failure. The staging-error test writes payloads before the format limit rejects
them, then checks that collection and subsequent intake still work.

```sh
cargo test --lib
cargo test --bench nar_import -- --test
cargo bench --bench nar_import
cargo clippy --all-features --all-targets -- -D warnings
cargo fmt --all -- --check
```

The retained real-closure command uses each binary recorded in `results.json`:

```sh
RUST_LOG='casita=debug,obrador_core=debug' \
strace -f -qq -ttt -T -yy -e trace=fsync,fdatasync -o trace.strace \
  BINARY /tmp/obrador-real-nar-seed-20260918 \
  /nix/store/sacz532zgiacvg7mva9v6gbfmyw427i3-gnugrep-3.12 OUTPUT_DIRECTORY 1 \
  > run.log 2> trace.jsonl
```

Use Obrador's `benchmarks/profile-nar-closure.py` to partition the trace. Next,
trace publication and lease lifecycle spans to explain the remaining calls
outside the writer stages before changing their durability boundaries.

Validation passed: 710 library tests (40 ignored), all 41 NAR benchmark
correctness cases, all-features/all-targets Clippy with warnings denied,
formatting, and diff checks. The expanded Obrador streaming-memory test passed.
