# Mutation catalog retention

A long-lived mutation previously accumulated every catalog snapshot used by
publication. A native Obrador import reached the 64 MiB pin-inventory limit:
one active staging pin held 285 catalog versions totaling 52,915,488 bytes in a
snapshot taken shortly before failure. This was separate from the synchronous
Obrador import hold-release bug.

Catalog snapshots now receive operation-scoped reader pins. A publication hands
its pin to the cancellation-safe commit task, alongside the existing mutation
protection. Staged object and physical-resource protection remains attached to
the mutation.

Validation on Linux, based on Casita `4be9276041e3ee82adf3e05ae76f1a5824e9bc0c`
plus this change:

- The repeated-publication regression fails before the fix (`before.log`).
- All 86 repository tests pass, including cancellation, concurrent collection,
  revision races, crash recovery, and full-disk cases (`repository-tests.log`).
- The permanent `mutation-catalog` benchmark passes through `benchmark all`
  (`all-execution.json`, `linux.json`). Both 63 and 65 successive 1 MiB catalogs
  peak at 1 MiB of active catalog bytes and finish with an empty pin inventory.

The synthetic catalog witnesses are opaque metadata, with a real bounded file
ledger. They isolate the retention boundary from physical pack encoding. The
separate publication regression uses real packed local storage and verifies
payloads across collection. Saved timings use a debug test binary and include
inspection and release drains; they are correctness evidence, not an application
throughput comparison. Artifact hashes are included in the JSON.

Reproduce with:

```sh
cargo test --features cli --lib repository::mutation_catalog_tests
benchmark mutation-catalog --repetitions 1 --output results.json
benchmark all --suites mutation-catalog --profile smoke --repetitions 1 --output all-results
```
