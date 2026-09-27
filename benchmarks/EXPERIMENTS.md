# Optimization experiments, September 2026

Baseline production source: `08e9c88`. These are development experiments on a
shared Linux workstation, not release performance claims. Raw results,
executable hashes, source snapshots, and build logs are retained locally under
`benchmarks/results/optimization-2026-09/` (ignored by Git).

The measurements and original validation counts below predate integration onto
`96b2f08`. That revision independently added a combined validated-payload query;
the merged implementation uses it and retains the payload-identity check.
The timings describe the retained experiment binaries, not a benchmark of the
rebased tree against `96b2f08`.

## Method

The baseline executable was built from an archived clean `08e9c88` tree. Both
executables use Rust 1.96.0, all features, release optimization, and the same
resolved Cargo.lock. The repository does not track Cargo.lock; the experiment
retains the actual lock alongside its source snapshot. This isolates production
source changes from dependency changes. Initial exploratory runs cover all
three standard corpora and all eight repository operations, three warm-cache
repetitions each: 72 validated samples per executable. The final comparison
alternates baseline/candidate order between rounds using
`experiments/compare_local.py`. Every repository sample passes the harness's
fsck and restored-manifest gates.

`sync-warm` means syncing an edited snapshot after an earlier full sync, not a
pure no-op. `experiments/transfer.py` separately measures unchanged sync,
source-discovery counts, peak process RSS, and forced spill usage. It checks
full fsck and exact restored contents after each corpus. Codec and metadata
microbenchmarks live in `crates/casita/benches/optimization.rs` and validate the compared
representations before timing.

Other projects also ran compilation and benchmarks on the host. The initial
large-file checkout medians varied from 0.264 s to 5.293 s despite little relevant
code changing in that read path. Those exploratory wall-time ratios are not
used as evidence of a speedup or regression. Request counts, exact bytes,
correctness tests, alternating runs, and isolated algorithm comparisons carry
more weight than a single host-level timing.

## Retained implementation

1. **Bounded state decoding.** The former call reserved the corruption limit
   (256 MiB) for each decoded record. A sized single frame now allocates from its
   bounded frame size, with a reusable decoder. Unknown-size and concatenated
   frames use bounded streaming decoding. The regression test verifies a tiny
   record uses less than 1 KiB capacity and covers truncation, invalid headers,
   oversized output, empty frames, and concatenation. This removes virtual
   allocation overhead; it does not imply every former allocation consumed
   256 MiB of resident RAM.
2. **Lightweight unchanged-file recognition.** Reuse the existing payload-summary
   batch API and closure flags instead of materializing canonical records. No
   new database schema or state trait was needed. Recognition also checks that
   the remembered content identity agrees with the stored payload.
3. **Explicit incremental discovery.** `sync --incremental` and
   `TransferDiscovery::ReuseVerified` stop at identical records with complete
   verified destination closures. Existing APIs remain exhaustive. The frozen
   request encoding is unchanged. Tests cover missing source descendants,
   incomplete destinations, and both in-memory and spilled discovery.
4. **Verify slices before storage.** Generic slice staging verifies caller bytes,
   writes them once, and checks the returned physical identity. It avoids
   reopening and decompressing the new payload. A native Git SHA-256 regression
   test proves zero storage reads, equivalence to `stage_existing`, rejection of
   wrong content, and rejection of a writer returning the wrong digest.
5. **Reuse compression contexts.** Independent zstd frames reuse worker-local
   compressors. Compression stays on blocking workers; the physical format,
   levels, chunk boundaries, and dedup-before-compression order are unchanged.
   Tests alternate compression levels on the same worker. Contexts retain
   workspace memory per active worker, so the end-to-end comparison also records
   process RSS. Removing blocking dispatch is not part of this change.
6. **Bounded transfer discovery.** Reuse the existing spill queue and one
   recursive-visit set under one aggregate temporary-byte budget. Explicit
   shallow selections fit in the request's existing 4,096-item / 4 MiB bound;
   they do not require a second graph-sized spill database. A shallow selection can still be
   upgraded to recursive discovery. Exhausting the budget fails before roots
   move. CLI spill limits now reach sync's destination repository; a CLI
   regression test checks both zero-budget failure and successful forced spill.

The larger transfer workload exposed a reader-lock panic in Turso during
publication. The clean baseline reproduced the unchanged-sync case as well.
Discovery now releases its destination read transaction before every intermediate
or final publication, opening a fresh view when discovery resumes. The mutation
session keeps physical retention throughout. Both the 32,768-file unchanged-sync
reproduction and a 4,096-file update pass fsck/restore gates. The latter publishes
4,129 new objects across several batches; exhaustive mode discovers 33,025 objects
and incremental mode 4,353. Releasing only at final publication was insufficient,
so `experiments/large_edit.py` retains the intermediate-publication regression.
This corrects transfer's snapshot lifetime without changing Turso's WAL code.

## Final standard repository comparison

Three repetitions per operation/corpus/artifact, alternating executable order:
144 successful samples. Entries below are baseline → final implementation median
seconds, with exhaustive discovery. Incremental sync has separate coverage.

| Operation | Small files | Mixed | Large files |
| --- | ---: | ---: | ---: |
| cold-import | 0.535 → 0.582 | 0.230 → 0.249 | 0.389 → 0.382 |
| unchanged-import | 0.215 → 0.154 | 0.065 → 0.040 | 0.037 → 0.029 |
| edited-import | 0.495 → 0.478 | 0.193 → 0.179 | 0.245 → 0.268 |
| checkout | 0.177 → 0.151 | 0.130 → 0.126 | 0.321 → 0.335 |
| sync-cold | 1.279 → 0.857 | 0.409 → 0.336 | 0.544 → 0.512 |
| sync-warm | 0.285 → 0.148 | 0.096 → 0.054 | 0.044 → 0.035 |
| verify | 0.188 → 0.138 | 0.145 → 0.140 | 0.257 → 0.316 |
| collect | 0.416 → 0.137 | 0.094 → 0.062 | 0.044 → 0.029 |

These timings are development evidence on a shared host with a powersave
governor and only three observations per cell. They do not establish a universal
speedup. Query/codec comparisons, the compression ablation, exact discovery counts,
and byte accounting provide more specific evidence for the retained changes.

## Focused codec and metadata measurements

Criterion uses 20 samples per case, a 200 ms warmup, and a one-second measurement
window. These medians are diagnostic; raw confidence intervals are retained.

| Work | Earlier path | Selected path | Result |
| --- | ---: | ---: | --- |
| 1,024 full record lookups, actual local state | 23.670 ms | 6.832 ms | 3.46× faster |
| Recognition lookup, full records to payload summaries | 23.670 ms | 4.868 ms | 4.86× faster for this query |
| 4 KiB generic slice staging, warm duplicate payload | 52.032 µs | 39.285 µs | 1.32× faster |
| 128-byte frame, limit capacity vs frame capacity, same process | 14.191 µs | 0.309 µs | Allocation sizing dominates this synthetic case |
| 4 KiB frame, limit capacity vs reused decoder, same process | 24.447 µs | 4.016 µs | 6.09× faster in the isolated codec comparison |

The payload-summary query is about 1.40× faster than full-record lookup even
after the decoder improvement. Codec timing excludes SQL and repository work;
the synthetic reusable-decoder loop already knows the frame size, whereas the
production helper also validates frame boundaries and limits.

Chunk compression's reusable contexts reduce isolated median time roughly
8–26% across the tested sizes and compressible/random data. A second experiment
uses one retained executable with only fresh/reused chunk context selection
changed through `CASITA_EXPERIMENT_FRESH_CHUNKS`. The switch exists only in the
ignored experimental source snapshot, not in production. Six alternating runs
cover cold and edited imports over all three standard corpora: 36 validated
samples. Median large-file cold import is 0.336 s fresh / 0.309 s reused, and
edited import is 0.214 s / 0.196 s. Small-file cold import is 0.416 s / 0.429 s;
mixed cold import is effectively equal. This supports retaining context reuse
without claiming an across-the-board import speedup.

## Transfer scaling

Final executable, three repetitions per case. Both corpora pass full fsck and
exact restoration. The forced-spill threshold is 128 objects; the in-memory
case uses the default 250,000-object threshold.

| Files | Mode | Discovered objects | Median seconds | Peak RSS, median MiB | Peak temporary bytes |
| ---: | --- | ---: | ---: | ---: | ---: |
| 4,096 | exhaustive-memory | 4,129 | 0.097 | 42.4 | 0 |
| 4,096 | exhaustive-spill | 4,129 | 0.454 | 47.2 | 7,453,544 |
| 4,096 | incremental | 1 | 0.030 | 37.2 | 0 |
| 32,768 | exhaustive-memory | 33,025 | 0.836 | 81.9 | 0 |
| 32,768 | exhaustive-spill | 33,025 | 6.878 | 97.0 | 152,386,512 |
| 32,768 | incremental | 1 | 0.206 | 72.8 | 0 |

Incremental discovery removes the graph walk below verified reused subtrees;
process startup and catalog/database opening still cost time. Leaf and shallow
selections skip closure-status queries because they have no descendants to prune.
Cold and edited incremental sync additionally pass 12 standard-profile cases.

Forced spilling is a capacity option, not a speed optimization. At these sizes
it costs time and can increase process RSS because SQLite and pack caches also
consume memory. It bounds the graph-sized Rust structures and aggregate temporary
disk use, not total process RSS. Keep the default threshold for ordinary work.
The initial two-set design used 291,777,896 temporary bytes at 32,768 files; the
single-set design uses 152,386,512 bytes, about 48% less.

## S3 state experiment: retain the current engine and format policy

An ignored release test, `metadata::wal3::experiments::checkpoint_window_amplification`,
uses the actual logical-shard encoding, reads, compaction, and writes over local
Chroma storage. It starts with 16 shards and 16,384 objects, then adds 16 objects
per generation for 128 generations. It compares checkpoint windows using the
same final 18,432-object state and verifies sampled additions. This isolates
physical shard amplification; it excludes WAL records, network latency, leases,
and manifest requests and is not an end-to-end S3 throughput test.

| Checkpoint interval | Checkpoints | Shard GETs / PUTs | Shard bytes written | Peak encoded object overlay |
| --- | ---: | ---: | ---: | ---: |
| 9 commits (current count policy) | 15 | 240 / 240 | 25,416,144 | 12,672 |
| 64 commits | 2 | 32 / 32 | 3,464,064 | 90,112 |
| 128 commits | 1 | 16 / 16 | 1,781,248 | 180,224 |

The 64-commit window reduces shard PUT count 7.5× and shard bytes about 7.3× in
this workload, at about 7.1× the peak encoded object overlay. The current reader
rejects more than eight cumulative deltas; increasing the constant would break
existing readers. The 1 MiB encoded-tail limit and recovery/GC barriers are also
part of the decision. **No production checkpoint policy change is retained.** A
future versioned policy or compatible delta coalescing needs workload-specific
WAL/recovery measurements and concurrency tests; these shard-only numbers do
not justify silently changing the contract.

The separately buildable `experiments/slatedb` prototype checks SlateDB 0.16.0
atomic batches, snapshots, durable flushes, and writer ownership over an in-memory
object store. Atomic publication and snapshot isolation pass. Opening the second
writer fences the first (`Closed error: detected newer DB client`); Casita instead
allows competing handles to retry after `StaleRevision`. **SlateDB is rejected as
a drop-in replacement.** Adopting it would require a single-writer service or a
changed concurrency contract plus adapters for verification, retention, and GC.
The probe's publish latency is diagnostic, not a Casita performance comparison.
SlateDB stays outside Casita's dependency graph.

## Validation and artifacts

- Release CLI build with all features succeeds.
- All-feature tests pass: 449 library tests and 22 CLI tests, plus integration
  tests and doctests. The 14 ignored library tests are opt-in benchmarks or
  external-data stress tests; the new checkpoint experiment ran explicitly.
- Default-feature tests pass (379 library tests, plus integration/doctests).
  Portable build/tests pass (62 library tests, plus applicable integration/doctests).
- Clippy passes with `--all-features --all-targets -- -D warnings`; rustdoc passes
  with `RUSTDOCFLAGS='-D warnings'`. Formatting and `git diff --check` pass.
  Clippy ran explicitly; redundant shell-entry hooks were skipped.
- All 117 Python benchmark-harness tests pass. The documentation site builds,
  and its internal link/fragment checker passes. The standalone SlateDB probe
  builds in release mode and completes its capability checks.
- Final end-to-end coverage includes the 144-sample standard comparison, 12
  incremental cold/edited-sync samples, 18 unchanged/forced-spill samples, and
  both modes of the 32,768-file / 4,096-edit publication regression. Repository
  cases check full fsck and restoration, not just command exit status.

The compact machine-readable record is
[`experiments/results-2026-09.json`](experiments/results-2026-09.json).
The measured final release executable is retained as
`results/optimization-2026-09/candidate-publication`; subsequent builds may replace
`target/release/casita`. That directory also holds
`final-comparison/`, `final-transfer/`, `final-incremental.json`,
`large-edit-publication/`, codec estimates, checkpoint/SlateDB outputs, and
build/test logs. A source archive and artifact manifest retain the exact source,
lockfiles, executable identities, and hashes. Earlier failed experiments are
preserved alongside the accepted results; they are not counted as passing runs.

## Integration validation on `96b2f08`

Before publishing the rebased changes, the all-feature suite passed with 458
library tests and 22 CLI tests, plus integration tests and doctests. Default and
portable suites passed with 386 and 62 library tests respectively. All-target
Clippy, rustdoc with warnings denied, formatting, the portable build, all 123
Python tests, and the documentation build/link checks passed on Linux.

The rebuilt release binary (SHA-256
`ac4bd457d10e1e9d3ce4031a283054f8c83bacf0248207990932c22e81e158e0`)
passed all 27 current standard repository cases, six incremental sync cases,
the unchanged/forced-spill experiments at 4,096 and 32,768 files, and both modes
of the 32,768-file / 4,096-edit regression. Every case passed its integrity and
restoration gates. The spill experiment also passed with 1, 126, and 127 files
using the rebased debug binary, covering below, at, and above its threshold.
These are integration checks, not a new baseline/candidate timing comparison.
Their raw results are retained locally under `results/prepush-2026-09/`.

## Reproduction

Use the pinned development shell (`devenv shell -- bash`) and build outside the
measurement window. Keep the same resolved Cargo.lock for both source trees.
Retain executables before switching build revisions; identify them by SHA-256,
not by the working directory's Git label. In custom-binary harness runs, the
embedded worktree fields describe the invoking checkout; the retained binary
hashes and source snapshots establish actual artifact provenance.

```sh
cargo build --release --all-features --bin casita
cargo bench --all-features --bench optimization -- --warm-up-time 0.2 --measurement-time 1
python3 benchmarks/experiments/compare_local.py /path/to/baseline /path/to/candidate --output /path/to/results --rounds 3
python3 -m benchmarks.experiments.transfer --binary /path/to/candidate --output /path/to/transfer-results
python3 -m benchmarks.experiments.large_edit --binary /path/to/candidate --output /path/to/large-edit-results
python3 -m benchmarks.cli run repository --profile standard --implementations casita --operations sync-cold,sync-warm --cache-policies warm --repetitions 2 --no-build --casita-bin /path/to/candidate --casita-incremental-sync --output /path/to/incremental.json --report /path/to/incremental.md
cargo test --release --all-features --lib metadata::wal3::experiments::checkpoint_window_amplification -- --ignored --nocapture
cargo run --release --manifest-path benchmarks/experiments/slatedb/Cargo.toml
```

For the compression ablation, apply `experiments/fresh-chunks.patch` to an
isolated source copy and build it once. Run the standard repository suite with
`--operations cold-import,edited-import --repetitions 1`, alternating
`CASITA_EXPERIMENT_FRESH_CHUNKS=1` and `=0` in fresh/reused/reused/fresh/fresh/reused
order. The same executable and data recipes serve both variants. The patch adds
only an experiment switch; do not install it in the production source tree.

## Upstream evidence

- The zstd [bulk decompressor implementation](https://docs.rs/zstd/0.13.3/src/zstd/bulk/decompressor.rs.html)
  explains the capacity reservation; [frame-content-size metadata](https://docs.rs/zstd-safe/7.2.4/zstd_safe/fn.get_frame_content_size.html)
  provides the bounded size hint.
- The zstd [Compressor API](https://docs.rs/zstd/latest/zstd/bulk/struct.Compressor.html)
  supports context reuse. Tokio's [blocking-task documentation](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)
  explains why synchronous compression remains off the async executor.
- SlateDB's [architecture](https://slatedb.io/docs/get-started/introduction/)
  describes the single-writer design; its [Rust API](https://docs.rs/slatedb/latest/slatedb/struct.Db.html)
  specifies snapshots, batches, flushes, and writer fencing. The prototype tests
  these capabilities directly rather than treating feature descriptions as an
  equivalent Casita backend.
