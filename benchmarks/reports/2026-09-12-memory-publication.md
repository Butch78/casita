# Reuse unaffected indexes during memory publication

Memory publication previously cloned all four object-related indexes before
inspecting the mutation, even for application-record or catalog-only changes.
The new implementation initially shares those indexes. On the first mutable
access to an index, `Arc::make_mut` copies it into the private next generation.
Idempotent object, root and validation changes avoid mutable access. Collection
still validates the full retained set and filters private object, birth and
validation indexes; it reuses the unaffected root index.

Validation and entropy failure still precede visible state changes. Failed
multi-index mutations leave objects, roots, validation marks, application
records, catalogs, generations and revisions unchanged. Snapshot and GC
contracts are unchanged. This changes only the memory backend.

## Permanent corpus

`memory-publication` is registered in the manifest, revision runner and
`benchmark all`. The standard run covers 256, 4,096 and 16,384 entries, six
mutation kinds, two reader policies and three processes per case: 108 samples.
Each process measures 16 commits. Every entry starts with an object, root,
validation mark and application record; catalog bytes equal the entry count.

Held mode retains the original generation and a current snapshot during each
commit. None mode holds neither. Timing includes commit and release of the
per-commit snapshot, so destruction is not silently moved outside measurement.
Snapshot acquisition, fixture construction, mutation construction and audits
are outside timing. Each sample checks exact inventory, root/record/catalog
values and birth generations, including the held original views.

Selected medians of per-process averages, milliseconds per commit and release:

| Mutation / reader policy | 256 | 4,096 | 16,384 |
| --- | ---: | ---: | ---: |
| Catalog / held | 0.0018 | 0.0020 | 0.0021 |
| Idempotent / held | 0.0035 | 0.0028 | 0.0036 |
| Record / none | 0.0019 | 0.0022 | 0.0023 |
| Record / held | 0.0132 | 0.2717 | 3.4890 |
| Root / held | 0.0168 | 0.4298 | 5.3429 |
| Object / held | 0.0410 | 0.8672 | 10.3266 |
| Collection / held | 0.1962 | 7.9223 | 50.7714 |

These are candidate scaling measurements, not a comparison with an old binary.
Unchanged indexes no longer force publication cost to scale with the entire
repository. A changed index still requires a full copy, and long-lived readers
retain older generations. Large object/root updates and collection remain
expensive; this is not a persistent-tree implementation or a constant-time
publication guarantee. The Obrador comparison separately measures old/new
binaries with real recipes and complete cleanup.

[Raw standard results](../results/memory-publication-2026-09-12.json) include all
samples, process output, correctness gates and binary identity. The
[`benchmark all` smoke subset](../results/memory-publication-all-2026-09-12/execution.json)
also completed. No compilation ran alongside these measurements.

## Reproduction and checks

The source change is based on shared snapshots at `3b60e53`, which includes
Casita main through `045478c`. The complete candidate is `7a9349c`.
The probe was compiled with Rust 1.95.0 in Obrador's environment; ambient tool
versions recorded by the Python runner describe that shell, not the prebuilt
probe compiler. Build the test executable and use the path printed by Cargo:

```sh
cargo test --release --features cli --lib metadata
benchmark run memory-publication --profile standard --no-build \
  --probe-binary /path/to/casita-lib-test \
  --output benchmarks/results/memory-publication.json
benchmark all --suites memory-publication --profile smoke --repetitions 1 \
  --bin-dir /directory/containing/casita-lib-test \
  --output benchmarks/results/memory-publication-all
python3 -m unittest benchmarks.tests.test_all benchmarks.tests.test_revisions
```

129 affected Casita tests passed, including process-crash tests and the
strengthened multi-index rollback regression; 14 performance probes were
ignored. All 23 benchmark runner/revision tests passed. Clippy passed with all
features and targets and warnings denied, using Casita's required Rust 1.96.0
hook environment. Cache deletion by a separate process interrupted initial
builds; complete successful rebuilds used isolated build directories under
`/tmp`. Failed builds were not used for measurements.

## Real Obrador builds

The separate fresh-repository series ran two first-output calls per variant
and recipe, in reversed order, with 1,000 input roots. Both variants already
include shared snapshots and overlapping input retirement. Full overhead
including import was 32.60 → 28.89 ms median for hello and 56.29 → 54.98 ms for
jq. The latter difference is within the observed variation and does not
establish a meaningful new-output speedup. The 10 ms target remains unmet.

Repeating identical recipes in the same repository benefits more because
output objects already exist, and those idempotent additions no longer copy
indexes. That workload must not be substituted for new-output publication.
The earlier mixed first/repeat series was noisy and is retained in full,
including outliers. Raw package samples, source/binary identities and phase
summaries are retained in Obrador's `benchmarks/closure-publication-2026-09-12/`.
The next object-publication optimization needs to reduce copies of the indexes
that actually change, while retaining exact rollback and snapshot isolation.
