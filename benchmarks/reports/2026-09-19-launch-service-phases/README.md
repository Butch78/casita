# First-launch service costs after decoded reuse

## Method

The existing permanent `native-fskit-workloads` suite ran its standard matrix
with `--trace-read-ranges`, three repetitions, and no extra metadata files.
It compares the native Rust FSKit evaluation, production fuser, and host for
1/8/15/16/17 workers, shared and distinct paths, fresh-mount first batches and
immediate repeats. Commands are retained in `launch-phase-profile.py` and the
JSON report. Native extension and server artifacts were reused after the
harness verified their source identities. Production source hashes exactly
match the preceding stable-cache investigation.

All 90 trials and 2,052 child executions passed correctness and teardown gates.
Native read traces accounted for every callback, without dropped records or
read errors. Rootless mounting and no-TCP native operation passed. Cleanup
reported no errors. One-minute machine load ranged from 1.78 to 3.26.

The initial attempt stopped before any workload because the original GNU awk
store path was missing. Exact GNU tool store outputs were restored with
`nix-store --realise` from the Nix cache before rerunning. The failed report is
retained as `workloads-stable-cache-phases-missing-tools.json`. Successful fixture
SHA-256 hashes and snapshots match the preceding run.

## Shared-path results

Median first-batch times, milliseconds:

| Workers | Native wall | fuser wall | Host wall | Read service | Stream reads | Directory work | Reader-lock wait |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 402.108 | 1,040.094 | 236.258 | 13.124 | 6.709 | 57.862 | 0.017 |
| 8 | 428.474 | 1,060.494 | 257.877 | 18.479 | 8.746 | 40.933 | 0.031 |
| 15 | 448.719 | 1,105.688 | 272.825 | 19.123 | 9.692 | 37.499 | 0.042 |
| 16 | 445.544 | 1,132.429 | 279.167 | 18.735 | 9.689 | 36.207 | 0.044 |
| 17 | 455.244 | 1,103.630 | 285.383 | 20.167 | 10.101 | 36.690 | 0.044 |

The callback count remains `281 + 30 * (workers - 1)`. At 17 workers there are
761 callbacks and 13,383,104 returned bytes. The cache removes redundant decoding,
not requests from macOS. The reader opens once and has 760 cache hits.

Before decoded reuse, the corresponding traced 17-worker stream-read median was
506.417 ms. It is now 10.101 ms. This is a historical comparison, supported by
same-binary replay results in the preceding investigation, not a randomized
mounted before/after experiment.

Directory work is now larger than stream-read work. The remaining native versus
host wall difference is roughly 170 ms at 17 workers. These measurements do not
identify all of that gap: callback delivery, FSKit/framework handling, scheduler
waits, executable validation, and process launch are not separately timed.
Do not attribute the unexplained difference exclusively to any one component.
Read-service time includes opens and stream reads; those columns must not be
added together. Aggregate service timings across concurrent requests and
independent medians are not an exclusive partition of wall time.

## Distinct-path threshold

| Workers | Native wall ms | Read service ms | Open ms | Stream read ms | Reader opens (three runs) | Evictions (three runs) |
| ---: | ---: | ---: | ---: | ---: | --- | --- |
| 15 | 3,076.423 | 123.164 | 65.609 | 52.172 | 15 / 15 / 15 | 0 / 0 / 0 |
| 16 | 3,274.647 | 131.992 | 67.456 | 62.106 | 16 / 16 / 16 | 0 / 0 / 0 |
| 17 | 3,690.890 | 310.336 | 207.337 | 95.427 | 44 / 46 / 39 | 28 / 30 / 23 |

The native evaluation retains 16 readers. Above that limit, first-launch callbacks
reopen readers repeatedly and discard their decoded reuse state. Open and eviction
counts establish that replacement occurs; the experiment does not isolate its
full wall-time penalty from the extra executable and platform launch work.
The permanent workload matrix already covers both sides of this threshold.

## Phase-changing decoded-cache workload

`decoded-seek-replay` now includes `phase-below`, `phase-at`, and `phase-above`,
also reached by `benchmark all` through its existing manifest entry. Each case:

1. Reads twice from a trailing chunk of 2 MiB minus one byte, exactly 2 MiB, or
   2 MiB plus one byte, activating decoded reuse.
2. Requires the exact expected cache occupancy before switching phases.
3. Repeatedly seeks through a disjoint 2 MiB region in eight 256 KiB chunks.
4. Verifies every returned byte, capacity bounds, and reader release.

The initial region is last in the blob so read-ahead cannot warm the new region
before the transition. Setup timing is recorded separately; elapsed and decoder
phase metrics measure only the new working set. Both cache modes start with warm
compressed data. The same test binary runs both modes in randomized order.

### Phase-change findings

Standard profile, three repetitions, 17 cycles through the new working set.
Median times exclude initial-phase warmup:

| Initial trailing chunk | Initial cache occupancy | New phase, cache off ms | New phase, cache on ms | New-phase decodes with cache |
| --- | ---: | ---: | ---: | ---: |
| 2 MiB - 1 byte | 2,097,151 | 26.561 | 25.632 | 136 |
| 2 MiB | 2,097,152 | 27.116 | 25.761 | 137 |
| 2 MiB + 1 byte | 0 | 27.219 | 1.826 | 8 |

The stable admission policy behaves as designed but fails to adapt: obsolete
cached data blocks the new working set. This is a loss of reuse benefit, not a
measured slowdown versus uncached reads. The just-too-large initial chunk is
never cached, allowing the later working set to fit and making its measured
phase about 14x faster than the exact-capacity case. Both sides of this admission
cliff are now permanent cases with exact warm-occupancy assertions.

The original gains remain: measured launch replay is 44.176 ms uncached versus
3.454 ms cached, and the 65-chunk scan is 15.930 ms versus 2.132 ms. Any adaptive
replacement should retain these controls, especially the cyclic scan that
regressed with LRU.

The real-input matrix passed 114 samples and synthetic smoke passed 38. All six
packed-fetch correctness tests passed, as did 15 Python harness tests, Rust
format checking, and whitespace checks. The production implementation was not
changed during this investigation.

## Reproduce and validate

Run `launch-phase-profile.py` from the Mac checkout with `PYTHONPATH=.` after
sourcing the native evaluation environment. It obtains the exact tool paths and
verified existing bundle/server from `results/workloads-stable-cache.json`.
For the new replay build:

```sh
cargo test --release --features cli --lib --no-run --message-format=json > results/cache-phase-build.jsonl
bash results/run-cache-phases.sh
```

`run-cache-phases.sh` is retained beside this report; copy it into `results/`
first. It parses Cargo's executable receipt rather than assuming a binary hash.
From the repository root, validate the downloaded reports with:

```sh
python3 benchmarks/reports/2026-09-19-launch-service-phases/summarize.py
PYTHONPATH=. python3 benchmarks/reports/2026-09-19-launch-service-phases/replay_summary.py
```

## Next steps

1. Prefer addressing native reader-cache eviction above 16 distinct files. Add a
   capacity control and compare it against the current limit, while retaining
   the process-wide decoded-byte budget and reader-release gates. Cover both
   sides of any new reader-count limit in the permanent corpus.
2. Add an admission policy that can recognize a changed working set without
   reintroducing cyclic-scan thrashing. Compare it using the new phase cases and
   existing 63/64/65-chunk cases before changing the default.
3. Directory first-touch work and framework/callback overhead are the next
   launch-profile targets. Current service counters cannot identify all of the
   platform gap; increasing Rust decoding concurrency is not supported by these
   measurements.

Execution note: the SSH client remained connected after the mounted Python
process exited and the complete report and success log were written. After
confirming that no profiling Python process remained, the local SSH client was
terminated. This did not interrupt a trial or alter the report's cleanup gates.

## Compressed raw reports

Raw JSON and logs are stored as `.gz`. Before running the reproduction or
summary scripts, run `gzip -dk ./*.gz` in this report directory. This restores
the original filenames and exact report bytes; uncompressed copies are ignored.
