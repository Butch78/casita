# Object-scoped local reads — 2026-09-08

Ordinary `Repository::open(key)` now retains the selected object closure and its exact physical read plan. A live reader permits unrelated logical objects and packs to be reclaimed. Explicit `retained_reader()` sessions retain their snapshot-wide contract.

The current implementation adds durable admission work. The small smoke cases opened in roughly 6–10 ms with object protection, versus 3–4 ms with snapshot protection. This is a GC-progress improvement with an admission-latency cost. Process-owned reader protection remains the next performance step.

## Method and correctness

- Local packed storage, 128 KiB pack target, payload cache disabled, OS caches left warm.
- Seed unrelated garbage before admission, remove the selected root after opening, drop the originating repository handle, then collect through an independent handle.
- Verify exact logical removals and physical pack reclamation. Vacuum before consuming any lazy payload bytes, then verify the full stream and a seek. Drop the reader, flush, verify complete pin release and collect the selected object.
- The smoke matrix contains 24 cases: six payload sizes (0, 64, 65535, 65536, 65537, 1048593 bytes), two garbage counts (1, 4), and two retention modes. Sizes cover both sides of the 64 KiB inline-verification threshold.
- The larger run contains 12 cases: two sizes, 64 garbage objects, both modes, three repetitions in deterministic shuffled order.
- Additional unit gates cover directory closures, root replacement, cancellation during admission, shared-catalog changes, and a live/killed child process. Crashed reader pins remain durable until explicit recovery after the owner is known to have stopped.

## Larger-run medians

Each cell is a median of three samples. These are development measurements with substantial wall-time variation; use the raw observations and ledger counts when assessing coordination costs. They do not establish a scaling threshold or a release performance guarantee.

| Payload bytes | Mode | Admission ms | Resolve ms | Open ms | Handoff flush ms | Read ms | Release ms |
|---:|---|---:|---:|---:|---:|---:|---:|
| 64 | object | 11.170 | 0.500 | 11.722 | 4.316 | 0.005 | 3.432 |
| 64 | snapshot | 14.616 | 0.432 | 14.934 | 0.008 | 0.005 | 16.946 |
| 1048593 | object | 30.865 | 17.811 | 46.255 | 16.389 | 2.922 | 18.344 |
| 1048593 | snapshot | 14.859 | 0.331 | 15.194 | 0.008 | 3.292 | 15.336 |

Open includes admission and resolve. Handoff flush drains the temporary snapshot release and is measured separately from the returned-open latency. Read measures consumption after GC; bare chunks have already been decoded during resolve. Release includes the final lease flush. Root changes, fixture creation, audits, and vacuum are outside phase timings. RSS includes setup and audits.

Object mode removed 64 logical objects and **2,102,912 pack bytes** in every larger case. The selected 64-byte object retained 153 pack bytes; the 1,048,593-byte object retained 1,048,887 pack bytes. Snapshot mode removed zero objects and reclaimed zero pack bytes while its reader remained alive. GC timings perform different amounts of work in the two modes.

Object opens required **3 ledger revisions** for empty/bare chunks and **4** for multi-chunk data through handoff cleanup. Snapshot opens required **1**. Final reader release required **1** in both modes. Object admission takes a validated temporary snapshot pin, adds a closure pin, resolves from an isolated copy of the protected catalog, pins exact immutable pack locations for lazy reads, and releases the snapshot. This avoids relying on the shared catalog after admission. The temporary broad pin can conservatively retain extra data for a collection already in progress; the GC-progress gates run after handoff cleanup. Sustained concurrent admissions remain a reason to replace this conservative protocol with cheaper process-owned protection.

## Reproduce

```console
cargo test --release --all-features --lib --no-run --message-format=json
# Set PROBE to the emitted library test executable.
benchmark all --suites object-reads --profile smoke --repetitions 1 --bin-dir /path/to/bin --output benchmarks/results/object-reads-all
# /path/to/bin contains a copy of PROBE named casita-lib-test.
benchmark run object-reads --profile standard --sizes 64,1048593 --garbage-counts 64 --repetitions 3 --probe-binary "$PROBE" --no-build --output benchmarks/results/object-reads-scale.json
cargo test --all-features --lib object_read_tests -- --test-threads 1
```

The default `benchmark run object-reads` builds its own probe. `benchmark all` can also build its artifacts when `--bin-dir` is omitted. The suite is permanently registered in the manifest and revision runner, with checked JSON parsing and independent dashboard dimensions for payload size, garbage count, and retention mode.

Measured base revision: `ade46e004c58354e4ffbd959642a0eccd48ce4c7` with this implementation uncommitted. Optimized probe SHA-256: `8318bf05be08fd9614c7341de3324d3ba4c7c236ac05114940aee331b6830131`. Subsequent test, documentation, and type-alias cleanups leave the benchmarked behavior unchanged. Environment identity and every raw sample are retained below.

- [Smoke results](2026-09-08-object-reads-smoke.json)
- [All-suite completion ledger](2026-09-08-object-reads-all.json)
- [Larger-run results](2026-09-08-object-reads-scale.json)
- [Larger-run individual timings](2026-09-08-object-reads-scale.md)

## Validation

- Public API integration suites: 6 local/application tests and 2 S3 tests passed.
- All-feature library suite: 605 passed, 23 intentionally ignored, including every process-crash boundary.
- Permanent benchmark smoke matrix through `benchmark all`: all 24 cases passed.
- Larger matrix: all 12 cases passed; every object-mode case reclaimed the expected 64 objects and their unrelated packs.
- Python benchmark harness: 166 tests passed. Both saved result matrices normalize successfully without merging retention modes or payload sizes.
- All-feature/all-target Clippy with warnings denied, formatting, and whitespace checks passed.
