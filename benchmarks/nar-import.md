# Concurrent raw NAR intake

`nar_import` measures cold raw NAR intake into a fresh local repository with a
real durable pin ledger. Repository setup and a full stored-content scrub are
outside the measured interval. Every sample checks the canonical SHA-256, NAR
size, measured payload bytes, zero encoding passes during intake, and the scrub
result. Payloads are deterministic and unique per nonempty file.

The corpus includes 1, 15, 16, 17, 64, and 1,024 small files, empty files,
16,383/16,384/16,385-byte files around the first Bao sidecar,
65,535/65,536/65,537-byte files around the decoder pipe capacity, and
131,071/131,072/131,073-byte files around the default FastCDC minimum. These
cases cover both sides of the staging window and buffering thresholds.
The benchmark is registered in `benchmarks/manifest.json` and the Criterion
set run by `benchmark all`.

Run inside the development shell:

```sh
cargo bench --bench nar_import
cargo test --lib nar::tests::concurrent_ -- --nocapture
# One correctness-gated sample of each permanent case:
cargo bench --bench nar_import -- --test
```

The durable regression prints the number of journal syncs during a 256-file
import and requires fewer syncs than files. These are
ledger instrumentation counts, not a syscall trace or a throughput claim.
For an external syscall audit on Linux, trace the permanent benchmark binary
with `strace -f -yy -e trace=fsync,fdatasync` and select paths ending in
`casita.sqlite.online-pins`; retain the binary identity and full trace alongside
Criterion output when comparing revisions.

The importer polls at most 16 file stages concurrently and consumes their
results in archive order. Each stage reads through the existing bounded pipe;
large files remain streamed. Completed children are published before their
parent directories, and no association is stored until parsing, measurement,
and publication have all succeeded. In-flight stages borrow the mutation
session and are dropped on cancellation.

## Validation on 2026-09-18

The same durable 256-file regression measured 269 journal syncs with the
original sequential importer and 32 to 33 with the concurrent importer, about 88%
fewer. The sequential version fails the regression's fewer-syncs-than-files
gate. Both runs used the debug test profile on the same host; this establishes
a reduction in durable ledger writes, not a release-throughput improvement.
All 13 permanent benchmark cases passed in Criterion test mode using
`cargo test --bench nar_import -- --test`.

Concurrent admissions can combine in the staging pin's pending protection set
before reaching the ledger worker. The ledger's `max_group` counter can therefore
remain one even when multiple files share a durable admission; journal syncs
are the relevant metric here.

Validation passed: 689 library tests (40 ignored), all 35 NAR tests including
cancellation and input failures, all 13 benchmark correctness cases, 305 Python
harness tests (two skipped), all-features/all-targets Clippy with warnings denied,
formatting, and `git diff --check`.

## Drain staging groups before downstream writes

Nested persistent-store imports exposed a lock dependency missed by the flat
fixtures: a buffered file-stage future can hold the shared pin admission gate
across an await. If the consumer suspends that future while writing a directory
or publishing records, the downstream operation can wait for the same gate.

Intake now drains each bounded group of at most 16 events before directory
writes or publication. It retains concurrent durable file admission and archive
order, without leaving a gate-holding stage suspended during downstream work.
The durable nested-directory regression and benchmark cases cover 15, 16, and
17 children per directory. Memory-only nested tests do not reproduce durable
pin-journal waits, so the new regression uses a local repository and a timeout.

The regression fails on `1f741ea` with its 15-second deadline and passes after
the fix. All 36 NAR tests and all 16 permanent benchmark correctness cases pass,
as do all-features/all-targets Clippy, formatting, and diff checks. The first
full-library run passed 689 tests but exposed a paused-input publication case;
that case and the full NAR suite pass after allowing partially filled groups
to drain when no more events are immediately available.

## Admit Bao sidecars with blob identities

The streaming writer now includes the predictable Bao root storage path in the
blob's durable pin admission whenever the outboard is nonempty. The pinned object
store reuses that confirmed protection. Sidecar writes, file and directory syncs,
and publication ordering are unchanged; paged outboard children still acquire
protection when their content hashes become known.

The ledger regression covers both sides of the first sidecar threshold at 16 KiB
and the configured FastCDC minimum. Small packed blobs retain one ledger edit,
including their sidecar, and the general single-chunk path retains two. Duplicate
writes add no edits. Deletion-claim tests exercise both chunk and sidecar claims,
including cancellation, atomic refusal, and eventual pin release. The permanent
NAR corpus includes 32-file cases at 16,383, 16,384, and 16,385 bytes.

## Bao directory-sync batching experiment

The [2026-09-19 investigation](reports/2026-09-19-bao-directory-batches/README.md)
retains a rejected prototype, measured syscall counts, and timing samples.
Directory-sync counts fell, but timings were inconsistent and included large
regressions. Production behavior is unchanged. The permanent corpus now also
covers 15/16/17 files just above the Bao sidecar threshold, through the existing
`nar_import` entry in the manifest and `benchmark all`.

## Packed Bao roots

Coordinated repository writers stage Bao roots in immutable packs capped at
64 KiB, including their sorted footer. A separately authenticated radix index
maps blob identities to pack offsets. Index updates rewrite only affected paths;
the existing catalog commit publishes the new root atomically with payloads.
Standalone stores retain loose sidecars, and repository readers still accept
loose roots when their selected catalog has no packed location.

The corpus covers 127/128/129 first-sidecar files around the index leaf limit and
629/630/631 around the pack capacity for 64-byte outboards. These cases retain
the same canonical hash, measurement, and full scrub gates as other imports.

Pack contents and index nodes are authenticated before use. A selected missing
or corrupt packed object is an integrity error, including when a loose copy
exists. Old catalog pins retain their pack and index objects. Collection removes
orphan mappings, repacks affected packs when at most half their entries remain,
and reclaims obsolete objects only after catalog publication under the existing
revision-fenced deletion protocol. Paged outboard children retain their existing
format and collection rules.

Catalogs containing a packed index use `casitap2`; the sidecar root participates
in the catalog checksum. Catalogs without packed roots retain byte-compatible
`casitap1` encoding. Older Casita versions reject the new catalog version, so a
repository written with packed roots requires the new reader.

Changed index nodes are prepared in memory, then uploaded with concurrency
limited to 16. The catalog is published only after all uploads are durable.
Unchanged roots do not rewrite index objects.

Validation passed: 704 library tests (40 ignored), including publication and
paged-overwrite process-death matrices; all 28 permanent NAR benchmark cases;
and all-features/all-targets Clippy with warnings denied. The new regressions
cover catalog abort/retry, retained roots, sparse compaction, abandoned sidecars,
malformed routing and offsets, corrupt packed data with a valid loose copy,
repair, reopening, and collection after the final reader is released.

Sparse compaction compares surviving entries with the authenticated physical
footer, including across successive removals and replacements. The final
read-time repair adjustment passed all 23 repair tests: typed packed-metadata
corruption can regenerate proofs from independently verified local payloads,
while unrelated I/O errors propagate.

## Ready admission windows (2026-09-20)

The event channel now holds at most 16 events. Before polling a writer, intake
fills the staging window from events that are immediately available. A missing
pin request yields once before acquiring the admission gate, giving ready
siblings a chance to join its durable edit. No timer or full-window wait is
introduced, and files continue through bounded pipes.

Repeated 256-file imports reduced median journal syncs from 41 to 25. See the
[retained investigation](reports/2026-09-20-nar-admission/README.md) for controls,
binary hashes, memory bounds, and reproduction commands. The permanent corpus
adds this 256-file case and 512/2,048-file cases at the 64 KiB pipe limit.

The allocation regression also found that zstd output retained plaintext-sized
spare capacity in pack staging. Finished compressed vectors are now shrunk
before staging, so their allocation follows the encoded length used by pack
limits. The 32/128 MiB many-file heap cases both pass the same 24 MiB Rust-heap
growth limit after this fix.

## Bounded directory staging

Directory writes from each completed event window are polled concurrently,
then fully drained before publication. Results remain in post-order. Queued
canonical directory sizes sum to at most 256 KiB; a larger directory runs
alone. The existing 16-event window also bounds queue length. This is an
encoded-size budget, not a total importer heap bound: open traversal frames,
BTreeMap overhead, writer buffers, and the currently completed directory are
separate. No batch waits for more input.

The registered `nar_import` benchmark, included in `benchmark all`, covers
15/16/17 and 256 unique sibling directories, 15/16/17 nested directories, and
three large sibling directories containing 63/64/65 symlinks of 4,095 bytes.
With 13-byte names, the latter encode to 259,883, 264,008, and 268,133 bytes,
covering both sides of the 262,144-byte budget. Every case checks canonical NAR
hashes and independently scrubs stored contents.
