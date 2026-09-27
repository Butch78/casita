# Pin batching and bounded checkpoint retries

This implements the first changes from the [protocol investigation](../../pin-ledger-protocol.md).
Writer-owned records and durable reconciliation of indeterminate commits remain separate work.

## Behavior

`MetadataError::MaintenanceFenced` means checkpoint admission was refused before
submitting a logical commit. Repository mutation publication, catalog-only
publication and background catalog maintenance retry this typed outcome and
stale revisions within one budget per publication loop: at most 32 attempts and
a 30-second retry window, with jittered backoff capped at 250 ms. The deadline
is checked between attempts; it never cancels submitted storage work and is not
an end-to-end operation timeout. Backend-internal retry policies are unchanged.

Each mutation retry obtains a fresh pinned snapshot, reevaluates the original
root expectations and reconstructs the candidate. An exact-revision request
still returns stale if that revision changes. The original staging session and
its payload protection remain alive throughout the loop. Pre-append rejection
allows an aborted catalog preparation to restore its unpublished changes for
the next attempt. Generic transient errors and potentially committed outcomes
are not added to this retry policy. A lost-response test verifies that a
committed candidate is not replayed by the new loop. Durable receipts and
catalog ownership for indeterminate outcomes remain required follow-up work.

Loose chunk uploads now admit their logical chunk ID and known physical path
in one protection update before either probing or uploading. The small-blob
path includes its already known blob ID in that same update. Packed paths still
require protection when catalog lookup discovers their locations. No pin TTL,
release ordering, storage-task lifetime or collection barrier changes.

## Permanent benchmark

`small-blob-pins` is registered in `benchmarks/manifest.json` and included in
`benchmark all`. Both profiles now cover loose and packed storage at 64, 511,
512, 513, 2047, 2048 and 2049 bytes around the FastCDC minimum and maximum.
The retained before/after comparison used the first four sizes. Standard uses 64 and 256
objects; smoke uses 16. Every sample checks exact contents using a newly opened
payload store, duplicate-write identity, zero duplicate protection edits and
eventual release of all pins.

```sh
benchmark run small-blob-pins --profile smoke --repetitions 1 \
  --output /tmp/pin-batching.json --report /tmp/pin-batching.md
```

The comparison uses baseline `1f741ea03328690bd42b8e923f562c5f31c55190` in an
isolated worktree with only the extended benchmark fixture applied. Both builds
use the same retained dependency lockfile and `--release --features experimental`.
These measurements predate the move into `crates/casita`; the paths and hashes
in `provenance.json` identify the measured source tree, not the rebased tree.
The harness accepts these prebuilt libtest binaries using `--probe-binary PATH
--no-build`. Binary identities and raw probe output are recorded in each JSON.
Retained results: [before](before.json), [after](after.json),
[baseline fixture patch](baseline-fixture.patch), [dependency lockfile](Cargo.lock.gz)
and [source/lockfile hashes](provenance.json). The measurement builds used host
Rust 1.97.1; final all-feature linting used the development shell.

To reconstruct the baseline, create a detached worktree at that revision, apply
`baseline-fixture.patch`, decompress the retained lockfile to its `Cargo.lock`,
then build with `cargo test --locked --release --features experimental --lib
--no-run --message-format=json`. Run the permanent harness above against the
reported executable with `--probe-binary PATH --no-build`; both layouts must be
present in its results. A legacy packed-only executable is rejected rather than
mislabelled as a loose-store measurement.

For 16 staged blobs, the expected and observed protection edit counts are:

| Layout | Bytes/blob | Before | After | Reduction |
|---|---:|---:|---:|---:|
| Loose | 64, 511 | 32 | 16 | 50% |
| Loose | 512, 513 | 48 | 32 | 33.3% |
| Packed | 64, 511 | 16 | 16 | 0% |
| Packed | 512, 513 | 32 | 32 | 0% |

Registration and release are outside these staging counts. A changing remote
ledger edit normally costs one whole-inventory GET and conditional PUT before
contention or transport retries. Thus this change removes one such pair per
loose chunk in the measured path. Concurrent compiler activity prevents a
controlled wall-time comparison; the retained times are diagnostic only.

## Validation and next work

Regression coverage includes unchanged exact revisions, root changes during a
maintenance refusal, mixed stale/maintenance budget exhaustion, deadline
exhaustion, concurrent GC, caller cancellation while a commit task settles and
committed-but-lost responses. Deletion-claim tests cover both logical chunks and
physical loose paths, including cancellation. A RustFS integration test forces
a checkpoint at the tail threshold while GC holds the barrier, observes repeated
admission attempts, releases GC, and verifies the named output through a freshly
opened metadata store.

Completed validation:

* [Repository tests](repository-tests.log): 97 passed, two ignored, including
  the seven new retry schedules.
* [Pin tests](pin-tests.log): 80 passed, seven ignored, including cancellation
  and killed-publisher recovery.
* [Batching tests](batching-tests.log): two tests covering both layouts,
  threshold boundaries, logical/physical claims and cancelled waiters.
* [Typed barrier test](typed-fence-test.log) and [RustFS publication retry](rustfs-retry-test.log): passed.
* [Python harness](python-tests.log): 313 tests, two skipped, no failures.
* [All-feature/all-target Clippy](clippy.log) with `-D warnings`,
  [format check](format.log) and `git diff --check`: passed.

Next, measure the integrated publication path across independent RustFS writers.
Keep admission fairness and GC duty cycle in the writer-owned protocol work;
this batching change reduces requests but retains the shared ledger.
