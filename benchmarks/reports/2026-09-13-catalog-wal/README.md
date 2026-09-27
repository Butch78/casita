# Catalog publication and SQLite WAL growth

Investigation of current main `62ea96d5f167a03233a50c0a97773d0f25ea4ce1`, with
the permanent `catalog-wal` probe added. Production persistence is unchanged.
The affected Obrador store was not supplied for this investigation; the original
approximately 16 GB WAL / 427 MB blob-pack observation is not directly verified.

## Finding

Current Casita reproduces substantial catalog write amplification with the
already-fixed Turso engine. Even a metadata-only commit rewrites the overflow
pages of the catalog-bearing row. Retaining one original SQL snapshot prevents
checkpoint progress and turns that repeated traffic into linearly growing WAL
history. Without that reader, automatic checkpoint/restart bounds the observed
file footprint but does not eliminate the repeated writes.

For 32 commits, native x86_64 Linux on Btrfs with Rust 1.96.0:

| Production catalog fixture | Serialized root bytes | WAL bytes, no held reader | WAL bytes, held reader | WAL bytes, 48-byte reference control |
|---|---:|---:|---:|---:|
| One-manifest baseline | 275 | 131,872 | 131,872 | 131,872 |
| Delta below 1 MiB sealing limit | 1,048,307 | 4,268,352 | 34,146,592 | 131,872 |
| Delta above sealing limit | 276 | 131,872 | 131,872 | 131,872 |
| Base below 4 MiB externalization limit | 4,194,346 | 4,239,512 | 135,663,392 | 131,872 |
| Base above externalization limit | 98 | 131,872 | 131,872 | 131,872 |

Catalog resubmission and metadata-only commits have identical WAL footprints.
The retained-reader slopes are 259 WAL frames per commit for the large delta
root and 1029 for the large inline base, versus one frame for the reference
control. Each frame is 4096 bytes plus a 24-byte header. These ratios concern
SQL WAL traffic; they are not end-to-end I/O or speedup estimates.

Unheld large-delta cases reach 1036 frames every four commits and reuse the log;
large-base cases checkpoint after each 1029-frame commit. Held-reader PASSIVE
checkpoints report `(1, NULL, NULL)`. Releasing the reader and explicitly
truncating returns the WAL to zero in every case while preserving the exact
reopened state.

This establishes a mechanism capable of producing a multi-gigabyte WAL. It
does **not** establish that an old reader, a particular catalog size, or this
mechanism alone caused the original Obrador observation. Inspecting that store
and its process/snapshot lifetimes remains necessary for incident attribution.

## What the code establishes

- `src/sqlite.rs` stores the catalog in `repository_state.payload_catalog`.
  `src/metadata/sqlite.rs::commit_impl` updates revision, generation and
  `payload_catalog = COALESCE(?2, payload_catalog)` in one row on every commit.
  Metadata-only commits therefore also touch the row containing the catalog.
- `src/blob/pack.rs::build_index_catalog` stores checkpoints inline through
  4 MiB, then switches to immutable shard objects. `pack/delta.rs` permits up to
  1 MiB of inline delta bytes (and at most 1024 inline deltas) before sealing an
  immutable run. External components do not make the serialized root small in
  every case: it can still carry the inline base, deltas, and run routing.
- `repository/publication.rs` prepares physical catalog components before the
  metadata commit and finishes or rolls back the prepared publication afterward.
  `CatalogObjectPublication` and `LocalDurability` already provide immutable
  content-addressed uploads and local durable publication.
- `mark_catalog_objects` and `reclaim_catalog_objects_inner` mark external runs,
  checkpoints and shards reachable from current and pinned catalogs. Pins still
  carry whole serialized catalog witnesses (`PinResource::Catalog`), so moving
  only the SQLite value without changing the witness representation would leave
  another source of catalog copying and journal traffic.
- The pinned Turso revision is `dca55133caa690f90dcdd58d3c4329fb0703659c`.
  Its `core/storage/wal.rs::should_checkpoint` attempts maintenance above 1000
  unbackfilled frames. Casita also requests a truncating checkpoint after a
  filesystem import and during collection maintenance. A large WAL may contain
  uncheckpointed history or reusable high-water space; its file length alone
  does not measure total bytes written or prove a particular root cause.
- Turso's checkpoint opcode returns `(1, NULL, NULL)` when checkpointing fails;
  it does not always supply integer frame counts. Casita's
  `checkpoint_write_ahead_log` consumes but does not inspect the result row, so
  a successful return from `compact_transient_state` alone does not prove that
  the WAL was truncated. The benchmark checks the resulting file length.

## Experiment

The probe constructs real production-format catalogs using the production
encoder/publication code, with no payload objects materialized. Cases bracket
the 4 MiB base externalization threshold by one manifest entry, and the 1 MiB
delta sealing threshold with overhead allowance. A one-manifest case is the
small-catalog baseline. Fixture creation and catalog membership checks are
outside the timed SQL loop.
The base fixtures exercise the production forced-checkpoint branch directly;
they do not claim that every fresh import builds a 4 MiB inline base.

For each catalog it seeds a real `TursoMetadataStore`, truncates the seed WAL,
then repeats either catalog resubmission, metadata-only commits, or commits of
a synthetic 48-byte digest/generation/length reference. Each mode runs with and
without one original metadata snapshot held across all commits. Resubmission
deliberately supplies identical catalog bytes: it isolates the cost of the SQL
update without changing catalog content or adding payload traffic. The reference
control models SQL storage only; it does not implement durable external roots.

The probe records every post-commit WAL file length and the final PASSIVE
checkpoint result `(busy, log frames, checkpointed frames)`. It checks the held
snapshot, releases it, verifies that TRUNCATE succeeds, reopens the database,
and checks the exact catalog, revision and generation. Timings include metadata
commits and stat calls; they are not application-throughput claims. File lengths
are neither cumulative I/O counters nor allocated-block measurements.

## Reproduction

```sh
devenv shell cargo test --release --lib --no-run --message-format=json > /tmp/catalog-wal-build.json
# Use the executable from the casita lib compiler-artifact in that JSON stream.
devenv shell python3 -m benchmarks.cli run catalog-wal --profile standard \
  --probe-binary /absolute/path/to/casita-lib-test --no-build --repetitions 3 \
  --output /tmp/catalog-wal.json
devenv shell python3 -m unittest discover -s benchmarks/tests
devenv shell cargo clippy --all-features --all-targets -- -D warnings
```

`catalog-wal` is registered in `benchmarks/manifest.json`, the revision build
contracts, and `benchmark all`. Both profiles run all format-boundary, SQL-mode
and reader-lifetime combinations; smoke uses 8 commits and standard uses 32.

## Validation and artifacts

- `results.json`: all 90 standard processes passed (30 combinations, three
  repetitions). WAL byte counts were identical across all three repetitions.
  Raw stdout/stderr, every WAL observation, environment and executable SHA-256
  are retained. `results.md` is the generated table.
- `smoke.json`, `all-execution.json`, `all-artifacts.json`: all 30 smoke
  combinations passed through the actual `benchmark all --suites catalog-wal`
  dispatcher with the same frozen binary.
- `python-tests.log`: 239 harness tests ran, 237 passed and two skipped.
- `clippy.log`: all-feature/all-target Clippy passed with warnings denied.
  Rustfmt and `git diff --check` also passed. No production behavior was changed,
  and no full Rust regression suite or macOS run is claimed.
- `source-overlay.tar.gz` contains the benchmark implementation, registrations,
  tests and resolved Cargo.lock. Apply it over the main revision above to
  reproduce the measured code. `SHA256SUMS` fingerprints the saved artifacts.

Run the all-suite dispatch reproduction with a directory containing the built
test executable named `casita-lib-test`:

```sh
devenv shell python3 -m benchmarks.cli all --suites catalog-wal \
  --bin-dir /absolute/path/to/bin --profile smoke --repetitions 1 \
  --output /tmp/catalog-wal-all
```

This investigation uses byte counts for its conclusions. Background host work
was not excluded, so recorded durations should not be used as latency comparisons.
During probe development an incomplete manifest fixture and non-null checkpoint
counter assumption failed correctness checks; both were corrected before the
saved standard and smoke runs. The measured code preserves busy checkpoints'
unknown counters as null rather than treating them as zero.

## Proposed focused change

Introduce a versioned external-root descriptor containing a digest, generation
and encoded length, with the object path derived from the digest. The existing
SQLite BLOB column can hold this small descriptor; removing the SQL BLOB type
is not itself the architectural requirement. Durable catalog objects must exist
before the SQLite transaction commits the new descriptor.

Teach catalog loading and pin/GC traversal to resolve and authenticate the new
descriptor, including retaining the root object itself and all its children.
Use the small descriptor in pin witnesses as well. Cache immutable root reads
by digest. Retain compatibility with old inline witnesses while old snapshots
and pins exist, and fence incompatible writers before switching formats.

Migration should publish the old catalog as an immutable object first, then
atomically replace its SQLite witness under the publication lock and revision
check. A crash before metadata commit must leave the old witness usable; a crash
afterward must leave the new object durable. Orphan uploads must be collectible,
while roots reachable through old readers remain protected. Extend existing
publication crash checkpoints with missing/corrupt external roots, migration
interruption, concurrent readers/GC, and aborted publication cases.

Externalizing a full catalog root removes its repeated SQLite and pin-journal
copies, but still writes a full immutable root on each changed publication.
Further reducing root-object writes requires a bounded manifest/delta design;
the current sharded/run representation is a starting point, not proof that all
end-to-end write amplification disappears.
