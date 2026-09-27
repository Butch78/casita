# Share memory metadata snapshot indexes

`MemoryMetadataStore::snapshot` previously copied every birth, object, root and
validation index, plus the optional payload catalog, for every reader. Records
already shared an immutable index. The change gives the remaining indexes the
same ownership model: snapshots clone `Arc`s, while publication constructs and
validates a private next generation before replacing the current state.

No API or persistent backend changes. Failed publication cannot alter a visible
revision. Old snapshots retain their exact objects, birth generations, roots,
validation marks and catalog across later publication and collection. The last
owner of an obsolete generation still pays its destruction cost; this is not
a guarantee that every possible snapshot drop takes constant time.

## Scaling measurements

Three fresh processes per count and variant, with 32 operations per phase.
Entries have one blob, root and validation mark each; catalog bytes equal the
entry count. Values are medians of process averages, in milliseconds/operation.
Fixture creation and correctness audits are outside the phase timers; process
RSS includes them. No compilation ran alongside these measurements.

| Entries | Previous snapshot algorithm | Shared snapshot | Publication with held reader |
| ---: | ---: | ---: | ---: |
| 256 | 0.058363 | 0.000111 | 0.065561 |
| 4,096 | 1.263621 | 0.001261 | 1.439268 |
| 16,384 | 14.784674 | 0.006567 | 12.215096 |

The previous algorithm is a test-only deep-copy control in the same binary,
not a separately built old revision. Both variants use the current commit
algorithm; their publication medians were 0.062889/0.065561, 1.382388/1.439268 and
12.669684/12.215096 ms respectively. These publication differences do not
establish a speedup or regression. Publication still copies indexes and scales
with repository size; long-lived readers still retain older generations.

The source-level removal of repeated full-index copies and the snapshot
measurements justify this small ownership change. They do not establish a
persistent-store improvement or an end-to-end sandbox timing bound.

Raw standard results, binary identity, process output and correctness gates:
[JSON](../results/memory-snapshots-2026-09-12.json),
[table](../results/memory-snapshots-2026-09-12.md).
The registered `benchmark all` smoke run also
[completed](../results/memory-snapshots-all-2026-09-12/execution.json).

## Reproduction and checks

Based on Casita `1778cf6be51d09c8a7d3438e019d97e6093d1ef7`, the Obrador pin at the
start of this investigation. Build the library test executable and use the
path printed by Cargo:

These binaries were compiled in Obrador's development environment with Rust
1.95.0. The result JSON's ambient tool versions (1.97.1) describe the shell
running the prebuilt probe, not its compiler. The full Obrador comparison uses
the same locked dependency versions on both sides. Casita's standalone probe
uses its own resolution; its deep-copy and shared variants are in one binary.
The [source identity](../results/memory-snapshots-source-2026-09-12.json) and
[probe lockfile](../results/memory-snapshots-probe-2026-09-12.lock) are retained;
copy the latter to `Cargo.lock` to reproduce that dependency resolution.

```sh
cargo test --release --features cli --lib metadata
benchmark run memory-snapshots --profile standard --no-build \
  --probe-binary /path/to/casita-lib-test \
  --output benchmarks/results/memory-snapshots.json
benchmark all --suites memory-snapshots --profile smoke --repetitions 1 \
  --bin-dir /directory/containing/casita-lib-test \
  --output benchmarks/results/memory-snapshots-all
python3 -m unittest benchmarks.tests.test_all benchmarks.tests.test_revisions
```

The affected Casita test selection passed 120 tests (12 ignored performance
probes). The new regression explicitly holds a snapshot across a failed
publication, root removal, catalog replacement and object collection. Existing
generation tests cover exact births and reinsertion. All 23 benchmark runner
and revision-contract tests passed. Obrador's 101 build and 180 core tests also
passed against the patched dependency, followed by the extended cancellation
and failed-import cleanup regression.
