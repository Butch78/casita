# Share memory-index nodes between revisions

The previous change shared whole indexes, but inserting a new object still
copied every object and birth entry. Memory metadata now uses `imbl` 7.0.2
ordered trees: each revision shares unchanged nodes and mutations copy only
the paths they touch. Objects, births, roots, validation marks and application
records all use this representation. Catalog bytes remain separately shared.
See the [library's ordered-map documentation](https://docs.rs/imbl/7.0.2/imbl/ordmap/struct.GenericOrdMap.html).

This is default behavior for the memory backend. Disk-backed metadata is
unchanged. Validation still runs in a private generation; revision entropy is
obtained before visible application-record updates. Exact collection removes
object and validation entries together, while old snapshots keep their view.
The Cargo and Nix pins are recorded in [source-metadata.json](https://github.com/cachix/obrador/blob/main/benchmarks/closure-index-2026-09-12/source-metadata.json).

## Fresh recipe results

Four calls per variant and recipe, one fresh in-memory repository per process,
alternating variant order across four batches. Each has 1,000 input roots, two
build cores, current-thread Tokio and tmpfs scratch. Full call minus recipe is
the primary overhead: it includes first-use service startup, output import,
input retirement and complete workspace cleanup. Input-fixture ingestion is
outside this timer. There are no repeated-output calls in this comparison.

Median milliseconds, with every observed value included in the range:

| Full overhead including output import | Whole-index copying | Shared tree nodes |
| --- | ---: | ---: |
| hello | 26.11 (24.22–28.43) | 25.03 (23.24–29.42) |
| jq | 41.68 (39.52–43.82) | 26.65 (23.50–30.85) |

Jq improved by about 36% in this sample, with disjoint observed ranges. Hello's
small median difference is within the variation and does not establish an
end-to-end improvement. **The 10 ms entire launch-and-cleanup target remains
unmet for both recipes.** These are memory-backend measurements; they do not
establish gains for the persistent backend.

For jq, median `state.commit` fell from 23.52 to 0.68 ms, output import from
29.46 to 4.74 ms, and input metadata destruction from 4.06 to 0.46 ms.
Import and retirement overlap, so those reductions must not be summed.
Retirement now outlasts import: kernel cache invalidation takes 11.11 ms and
total input retirement 11.79 ms. Prepared-bubblewrap setup is another 5.43 ms.
The next useful launch/cleanup experiment should reduce cache invalidation or
sandbox setup work. Faster metadata commits alone cannot meet the target.

All 16 calls retained the expected output NAR sizes: hello 274,568 bytes and
jq 568,808 bytes. The runner disables fusermount helpers, and the timer checks
zero builder capabilities and NoNewPrivs. The harness rejects a workspace
left behind at return. No host root help or configuration changes were used;
private input edits remain isolated from Casita originals.
[Raw calls, commands, hashes, timestamps and host load](https://github.com/cachix/obrador/blob/main/benchmarks/closure-index-2026-09-12/fresh/paired-metadata.json);
[stage summaries](https://github.com/cachix/obrador/blob/main/benchmarks/closure-index-2026-09-12/fresh/summary.json). No calls were discarded.

## Commit scaling and counterweights

Three processes per case, 16 commits per publication process. Timing includes
commit and releasing the per-commit reader; held mode also retains the original
revision. Values below are median milliseconds per commit.

| Mutation / readers | Entries | Whole-index copying | Shared tree nodes |
| --- | ---: | ---: | ---: |
| object/held | 256 | 0.04000 | 0.00571 |
| object/held | 4,096 | 1.07013 | 0.01469 |
| object/held | 16,384 | 11.68031 | 0.02679 |
| object/none | 16,384 | 13.22358 | 0.02901 |
| root/held | 16,384 | 8.15009 | 0.01002 |
| record/held | 16,384 | 5.13582 | 0.00928 |
| record/none | 16,384 | 0.00245 | 0.00245 |
| idempotent/held | 16,384 | 0.00349 | 0.00482 |

A new-object commit with a held reader is about 436 times faster at 16,384
objects; that ratio describes this microbenchmark, not whole builds.
Idempotent commits increased from 3.49 to 4.82 microseconds in this sample;
record updates without a reader remain about 2.45 microseconds.
[Publication baseline](https://github.com/cachix/obrador/blob/main/benchmarks/closure-index-2026-09-12/publication-baseline.json), [candidate](https://github.com/cachix/obrador/blob/main/benchmarks/closure-index-2026-09-12/publication-paths.json).

The new permanent `memory-index-lifecycle` suite separately measures seeding,
all-object/application-record lookups, ordered scans in 128-record pages,
collection and dropping the last store/snapshot owners. Lookup timing includes
value checks; fixture construction and post-collection audits are excluded.
It covers 256, 4,096 and 16,384 entries, 0/1/50/99/100 percent removal, and both
held-reader/no-reader policies. Three processes per case give 90 processes and
450 phase samples per variant. Both variants passed every correctness gate.

At 16,384 entries with an old snapshot held, median milliseconds:

| Phase | Whole-index copying | Shared tree nodes |
| --- | ---: | ---: |
| Seed (50% removable) | 64.75 | 61.34 |
| All-key lookup | 21.19 | 17.63 |
| Ordered scan | 4.50 | 4.54 |
| Collection: 0% removed | 56.64 | 34.13 |
| Collection: 1% removed | 53.85 | 31.55 |
| Collection: 50% removed | 37.00 | 36.13 |
| Collection: 99% removed | 29.94 | 22.91 |
| Collection: 100% removed | 18.05 | 15.07 |
| Last-owner drop (50% removed) | 18.16 | 16.27 |

The measured read/collection cases showed no consistent regression across sizes,
but individual cases were slower: removing all 256 objects without a held reader
was 0.107 → 0.185 ms; lookup at 4,096 entries with all objects retained and a held
reader was 2.76 → 3.54 ms; final-owner drop at 16,384 entries with all retained
and no reader was 10.65 → 13.66 ms. These three-process cases remain in the raw
results; shared trees do not win every operation. Collection
still scans and validates the retained graph, and dense deletion still performs
individual tree removals. Those costs are not eliminated. Median peak RSS for
the 50%-removal/held case was 51.84 versus 49.14 MiB; that includes seeding and
the probe's independent expected-value maps, not just index memory.
[Lifecycle baseline](https://github.com/cachix/obrador/blob/main/benchmarks/closure-index-2026-09-12/lifecycle-baseline.json), [candidate](https://github.com/cachix/obrador/blob/main/benchmarks/closure-index-2026-09-12/lifecycle-paths.json).

The change adds six native-profile dependencies. The measured release executable
grew by 108,672 bytes, about 0.3%. The existing snapshot suite keeps a BTreeMap
clone control built before timing so the control does not accidentally become
a cheap shared-tree clone after this representation change.

## Validation and reproduction

130 affected Casita tests passed (15 ignored probes), including crash recovery
and a new pagination test that checks 33 held generations against independent
ordered maps after scattered record updates and deletions. All 101 Obrador
build and 181 core tests passed (2 ignored). Casita Clippy passed with all
features/targets and warnings denied. The 23 benchmark runner/revision tests
and the registered `benchmark all` lifecycle smoke passed. Smoke timing was
used only as a correctness check, not included in the comparison above.

Both measured variants use Rust 1.95.0 release builds. Casita Clippy uses Rust
1.96.0. Ambient tool versions in the Python microbenchmark describe the runner
shell, not the compiler of its prebuilt executable. Microbenchmark variants ran
sequentially with the same shuffled case schedule; recipe variants alternated.
No compilation we launched ran during timed comparisons. Other activity on this
shared development host was not controlled; all outliers remain in the records.

The Casita baseline is `af6767d`, plus [baseline-probe.patch](https://github.com/cachix/obrador/blob/main/benchmarks/closure-index-2026-09-12/baseline-probe.patch)
which changes test probes only. [casita-index.patch](https://github.com/cachix/obrador/blob/main/benchmarks/closure-index-2026-09-12/casita-index.patch) records
the production/dependency and pagination-regression delta against that revision.
The permanent corpus is committed in Casita. Obrador's retained baseline binary
has the same Rust and bubblewrap-helper sources as `c063dea`, verified by hash;
the candidate was built from that revision with a command-line Casita source
patch. Its locked graph changes only Casita's source and the six new dependencies.
The production pin is updated after source identity validation.

```sh
# In Casita, once for each prebuilt baseline/candidate test binary:
python3 -m benchmarks.cli run memory-publication --profile standard \
  --probe-binary /path/to/probe --no-build --output /path/to/publication.json
python3 -m benchmarks.cli run memory-index-lifecycle --profile standard \
  --probe-binary /path/to/probe --no-build --output /path/to/lifecycle.json
python3 -m benchmarks.cli all --suites memory-index-lifecycle --profile smoke \
  --bin-dir /path/to/directory-containing-casita-lib-test --output /path/to/smoke

# In Obrador:
python3 benchmarks/compare-closure-retirement.py \
  --baseline /path/to/baseline --candidate /path/to/candidate \
  --candidate-label paths --timer /path/to/obrador-closure-timer \
  --fixtures benchmarks/closure-overlay-2026-09-12 --rounds 1 --batches 4 \
  --output /path/to/fresh
python3 benchmarks/summarize-closure-comparison.py /path/to/fresh
```
