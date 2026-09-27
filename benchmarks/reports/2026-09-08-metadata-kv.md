# Local metadata primitives: implementation and measurements

Implemented namespaced opaque record reads, indexed prefix pagination, and expected-value commits over records and GC roots. Application records are supported on local Turso and memory. S3 supports root-only commits through its existing publication protocol and rejects application-record operations.

## Measurements

Medians of three alternating fresh-process repetitions, each with ten timed operations: 8,192 path records, 257 matching referrers, 16-key batches, 256-byte values, and 16-record scan pages. OS caches were not flushed. Reads reuse a stable metadata reader. Compilation had finished before these runs.

| Operation | Before | After |
|---|---:|---:|
| get-batch | 52.83 µs | 49.87 µs |
| get-batch-scalar | 274.86 µs | 265.96 µs |
| scan-first | 28.50 µs | 27.90 µs |
| scan-deep | 20.18 µs | 19.00 µs |
| commit-insert | 1430.70 µs | 1389.74 µs |
| commit-register-root | 10837.08 µs | 1351.15 µs |

Root registration (one descriptor, 16 reverse references and one root) improved 8.02×. In the final build, the mixed batch read is 5.33× faster than identical scalar reads. These are local medians, not universal latency promises or reliable p99 estimates.

Root promotion uses an indexed check that the target is currently rooted and has a persisted verification witness. This preserves protection even during emergency collection, which may delete unrooted payloads before logical pruning. Rootless or unverified targets still require pinned admission and full verification. Existing-content fallback publication skips the payload/catalog flush; staged content still follows full durable publication.

## Validation and scaling

- The final smoke matrix ran through `benchmark all`: eight configurations, including 8,191/8,192 records with batches of 1/16.
- The standard matrix passed 24 configurations: 256, 257, 8,191, 8,192, 65,536 and 65,537 path records crossed with batches of 1, 16, 256 and 257.
- Twelve additional configurations passed with 4 KiB values and page sizes of 256/257.
- All runs require exact values and ordered prefixes, no partial checked writes, correct concurrent winners, persisted indexes, and GC-root separation. Failed processes cannot contribute successful samples.
- Tests cover binary prefixes, snapshot/cursor and byte limits, independent handles, rootless-target admission during an emergency fence, semantic graph verification, migration, and deterministic process death around record/root transactions.

## Collection regression found during the investigation

Two preliminary standard attempts failed at 8,192 records and batch 1 with Turso’s `reader slot released by non-owner` assertion during collection-plan teardown. Draining background leases before reopen did not resolve it. Their failed receipts are retained in `excluded_attempts` and excluded from the table.

Collection retained its mark snapshot after all snapshot reads were finished. It now releases that read transaction before prune/sweep publication can checkpoint the WAL, retaining the expected revision, frozen sets, and collector protection. The formerly failing case and its 8,191-record neighbor pass with batches of 1/16. A separate 8,192-record test keeps an application metadata reader alive across writes and collection and verifies its old values remain stable.

## Reproduction

```console
benchmark all --suites metadata-kv --profile smoke --output benchmarks/results/metadata-kv-all
benchmark run metadata-kv --profile standard --repetitions 1 --output benchmarks/results/metadata-kv-standard.json
benchmark run metadata-kv --counts 8191,8192 --batches 1,16 --iterations 10 --repetitions 1 --output benchmarks/results/metadata-kv-collection-regression.json
benchmark run metadata-kv --counts 256,257 --batches 16,256,257 --value-bytes 4096 --page-size 256 --iterations 3 --repetitions 1 --output benchmarks/results/metadata-kv-page-256.json
benchmark run metadata-kv --counts 256,257 --batches 16,256,257 --value-bytes 4096 --page-size 257 --iterations 3 --repetitions 1 --output benchmarks/results/metadata-kv-page-257.json
```

Exact before/after commands, environment metadata, SHA-256 binary identities, per-process receipts and raw timings are retained in [the JSON report](2026-09-08-metadata-kv.json). Before/after binaries came from successive uncommitted implementations in this worktree; the retained artifact hashes, rather than Git HEAD alone, identify them. Benchmark binaries live in the local ignored result directories; the permanent source corpus builds the final implementation.
