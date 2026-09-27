# External local catalog roots

This change addresses the SQLite catalog write amplification reproduced in
`../2026-09-13-catalog-wal/README.md`. It applies to the standard local packed
repository profile. WAL3 and standalone advisory catalog publication retain
their existing representation.

## Measured result

All 120 standard processes passed (40 combinations, three repetitions).
Every repeated SQL/WAL byte count was identical. All external cases stored a
56-byte witness and reached 131,872 WAL bytes after 32 commits, regardless of
catalog boundary or retained reader. Each released WAL truncated to zero.

| Catalog case | Held reader | Inline SQL witness | Inline WAL bytes | External SQL witness | External WAL bytes |
|---|---|---:|---:|---:|---:|
| delta-below | False | 1,048,307 | 4,268,352 | 56 | 131,872 |
| delta-below | True | 1,048,307 | 34,146,592 | 56 | 131,872 |
| base-below | False | 4,194,346 | 4,239,512 | 56 | 131,872 |
| base-below | True | 4,194,346 | 135,663,392 | 56 | 131,872 |

The above-threshold cases remain in the corpus: their existing external
components already kept the inline root small (276 or 98 bytes). The fix
makes the SQLite witness uniformly small on both sides of those boundaries.
The inline metadata-only controls reproduce the same WAL growth as explicit
catalog resubmission. `results.json` retains raw output, per-commit WAL sizes,
checkpoint results, configuration, environment, and executable identity.

## Persistence and compatibility

The SQLite catalog witness and newly admitted local pin catalog witnesses contain 56 bytes:
`casitae1` (8 bytes), catalog generation (little-endian u64), encoded root length
(little-endian u64), and the BLAKE3 digest (32 bytes). The digest identifies the
immutable v1 root under `blobs/pack-indexes/b3/<xx>/<hex>`; that root continues to
describe its inline base/deltas and external checkpoint, shard and run objects.

Publication durably writes dependencies and the root before committing the
reference with logical metadata. The existing cancellation-safe commit task
owns the prepared publication and its staging pins. Failures leave the previous
reference authoritative and the candidate eligible for subsequent reclamation.

Schema 6 renames `payload_catalog` to `payload_catalog_ref`. Schema-4 and
schema-5 inputs migrate on open. The rename invalidates legacy catalog UPDATEs,
including prepared statements on already-open writers; it is not just an
advisory version field. Local initialization then migrates an inline witness
under the exclusive filesystem publication lease. A cancellation-safe task
keeps that lease through root upload and metadata commit. A killed process
leaves either the inline witness or a reference to a durable root; reopen retries
as needed. Already migrated repositories need no migration writes at open.

Migration needs writable space for the durable root and the SQLite update. It
prevents subsequent catalog BLOB rewrites; it does not promise to shrink an
already-grown database or WAL immediately. Existing reader snapshots still
control when accumulated WAL history can be checkpointed.

Old inline pin witnesses remain readable. GC marks external root objects as
well as all reachable catalog components from current state and pin witnesses.
An unchanged authenticated reference reuses the in-memory catalog. A new open
resolves and checks the root's exact length, digest, and generation. Missing,
corrupt, nested, malformed or oversized external roots fail closed; inventory
discovery cannot replace committed state.

## Permanent benchmark

The `catalog-wal` corpus retains its five cases bracketing the 1 MiB delta and
4 MiB base thresholds and its retained/unretained reader controls. The new
`external` mode copies fixture dependencies to a real local store before timing,
then durably publishes the root and commits the real 56-byte descriptor on each
iteration. It authenticates the external root after reopening the SQLite state.
The old `reference` mode remains a separate 48-byte SQL-only control.

The saved measurements use the default-feature, unoptimized library test binary.
It was built with the pinned devenv Rust 1.96.0 toolchain (`build-toolchain.log`).
The Python 3.13.12 runner recorded Rust/Cargo 1.97.1 from its own PATH; those
environment fields do not identify the compiler used for the frozen executable.
The conclusions concern SQLite byte counts, not timing or import throughput.

The catalog bytes deliberately remain identical across these controlled
resubmissions. This isolates the SQL footprint and exercises the durable root
writer, but does not measure end-to-end import throughput, changing-root disk
retention or GC scheduling. External roots still need their own durable writes;
this change does not claim to eliminate all catalog-object write amplification.
Metadata-only publications with no payload changes do not republish the root.

```sh
devenv shell cargo test --lib --no-run --message-format=json > /tmp/external-catalog-build.json
# Copy the casita library test executable from compiler-artifact records to a fixed path.
devenv shell python3 -m benchmarks.cli run catalog-wal --profile standard \
  --probe-binary /absolute/path/to/casita-lib-test --no-build --repetitions 3 \
  --output /tmp/external-catalog.json
devenv shell cargo test --all-features
devenv shell python3 -m unittest discover -s benchmarks/tests
devenv shell cargo clippy --all-features --all-targets -- -D warnings
```

## Correctness coverage

New tests exercise descriptor bounds, length/digest/generation authentication,
missing and corrupt roots, aborted root publication and retry, GC of orphan
roots, retention of historical roots, and prepared legacy-writer fencing.
The existing repeated-publication/GC test now asserts that both SQLite and
all active local pin catalog witnesses contain exactly 56 bytes.

The migration subprocess matrix discovers publication checkpoints, kills a
writer at each boundary, and reopens in a fresh process to verify acknowledged
payload bytes, the reference, collection and fsck. Existing repository and
rebase publication crash matrices also exercise external root writes. These
tests model process death; they do not certify actual device power loss.


## Full-disk recovery

A small SQLite row can reuse existing WAL space to commit logical pruning on a
completely full filesystem, while the subsequent physical cleanup or catalog
root upload still needs new space. The previous emergency path handled only a
failed logical prune. Collection now also recognizes typed allocation failures
after pruning, enters the existing durable recovery fence, revalidates payload
pins, reclaims stale physical data, and retries catalog publication. Generic
stores without the colocated local-storage guarantee do not use this fallback.

The subprocess test injects allocation failure both before and after logical
pruning, aborts after physical deletion, and verifies recovery, rooted bytes,
and fsck. The real full-filesystem crash test accepts either ordering while
retaining its physical deletion, rooted-data and abandoned-fence assertions.


## Concurrent admission during catalog cleanup

The all-feature multiprocess integration exposed a cleanup race: a new staging
pin could invalidate the catalog mark between deletion batches, returning
`CatalogPinsChanged` through mutation admission. External roots make catalog
cleanup due more often. Advisory catalog cleanup now uses the existing
post-publication payload-cleanup policy: finish already-owned deletion work,
defer the next batch on this exact typed signal, preserve the reclaim marker,
and release collector ownership normally. Other failures still propagate.
The lower-level statistics-returning collector retains its strict error result.

A deterministic regression test pauses the first deletion batch, admits a
catalog pin, and verifies successful deferral, settled claims, surviving roots,
and completion from a fresh mark. The original concurrent-process workload is
also rerun without weakening its assertions.

## Validation and saved evidence

- The full default-feature library suite passed: 625 tests, 36 ignored corpus
  probes. It used the final production code before the additional injected
  post-prune test branch was added. The final frozen binary then passed all
  seven focused checks, including both injected crash orderings and the real
  full-filesystem crash test.
- The optimized full-disk integration target passed all three real-filesystem
  tests: reclaimable garbage, insufficient reclaimable space, and failed spill
  allocation.
- The initial optimized all-feature run passed 737 library tests (41 ignored)
  and 31 CLI unit tests, then reached the concurrent-writer failure described
  above after the preceding integration targets passed. After the fix, 108
  focused regressions passed (6 ignored), including catalog cleanup, SQLite,
  interrupted migration, and full-disk recovery. The original multiprocess
  target passed all four tests, the remaining verified-CLI target passed, and
  all nine documentation tests passed. The concurrent-publication scenario
  then passed two further independent trials (three successes after the fix).
  Logs preserve the original failure and
  the targeted rerun; the entire all-feature suite was not repeated after this
  narrow cleanup change.
- All-feature, all-target Clippy passed with warnings denied. Rustfmt and
  `git diff --check` passed. All 240 Python benchmark harness tests passed.
- The 40-case smoke profile passed through the actual `benchmark all` dispatcher.
  `smoke.json`, `all-execution.json`, and `all-artifacts.json` retain the results,
  completion ledger, and frozen executable SHA-256.
- `source-overlay.tar.gz` and `source.json` preserve the measured standard-run
  source and resolved Cargo.lock. `final-source-overlay.tar.gz` and
  `final-source.json` add the advisory cleanup race fix and its regression test.
  That fix does not change the catalog-WAL probe path. The smoke profile is
  rerun from the final source; base revisions and source hashes are retained. Logs retain the test
  and tool outcomes. Some devenv logs contain a "no hooks found" wrapper message
  because the automatic Clippy hook was explicitly skipped; the requested
  commands ran afterward, including explicit all-feature Clippy.
