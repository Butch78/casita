# Durable NAR invalidation generation

`nar_generation` in `nar_associations` measures the successful steady-state
paths affected by persisting the invalidation generation. It is registered in
`benchmarks/manifest.json` under `core-primitives` and runs in `benchmark all`.

The eleven cases cover:

* Local cached `ensure_nar`, using the original reader or a reader from an
  independently opened repository handle, with 1 or 256 files of 1,024 bytes.
* Repeated local raw intake of those same archives, exercising deduplication
  and association merging while still hashing the incoming stream.
* Explicit local scrubs for both file counts, exercising the measurement path.
* One-file cached verification, repeated raw intake, and scrubbing in memory, as
  controls without the database I/O of the local backend.

Repository construction, initial import, acquiring retained readers, cache
warmup, assertion checks, report destruction, and the final stored-content scrub
are outside the timed interval. Timed raw intake includes its normal durable
publication and pin coordination. Every result must match the independently
computed canonical SHA-256 and archive size, and report either the full payload
hashing count or an association hit without hashing.
Explicit scrubs perform one encoding pass; other operations perform zero.
An independent stored-content scrub validates each completed case.

These are warm, sequential successful operations on a current-thread Tokio
runtime. The second-handle case excludes opening the handle and is not a
concurrency benchmark. The cases do not measure corruption handling, contention,
full physical audits, cold disks, or large streaming files. Both revisions use
identical archive fixtures and benchmark source; only production code differs.

```sh
# In the development shell:
cargo bench --bench nar_associations -- nar_generation --test
cargo bench --bench nar_associations -- nar_generation
benchmark all --suites core-primitives --output /tmp/core-primitives-results
python3 -m unittest benchmarks.tests.test_nar_generation benchmarks.tests.test_all
```

For a historical comparison, copy the current `nar_associations.rs` into the
baseline checkout, whose `benchmarks/fixtures/nar_import.rs` must match, and build
both with the same toolchain and default release profile. `Cargo.lock` is ignored
by Git here: copy it from the candidate into the baseline checkout and use
`--locked` so dependency versions cannot confound the comparison. Use separate
Cargo build directories for the two worktrees: sharing a `build.build-dir` can
reuse a stale executable across revisions. Retain each executable
before building the other revision. Cargo's JSON output identifies the actual
executable even when a user configuration relocates build artifacts:

```sh
cargo bench --locked --bench nar_associations --no-run --message-format=json > build.jsonl
python3 - <<'PY'
import json, shutil
rows = [json.loads(line) for line in open("build.jsonl")]
executable = next(row["executable"] for row in rows
                  if row.get("executable") and row.get("target", {}).get("name") == "nar_associations")
shutil.copy2(executable, "nar-associations-binary")
PY

# Supply the retained executable from each checkout and a new output directory.
python3 benchmarks/tools/compare_nar_generation.py \
  --baseline-binary /path/to/baseline-binary \
  --candidate-binary /path/to/candidate-binary \
  --repetitions 4 --output /tmp/nar-generation-comparison
```

The comparison runner first executes all correctness cases in both binaries,
then alternates baseline/candidate order by repetition. It retains executable
hashes, host information, system load, logs, per-process Criterion estimates,
and raw iteration/time arrays. Identical executables and missing or extra cases
fail the run. The summary
compares the median of each revision's process medians and reports both absolute
nanoseconds and percentage changes. Individual Criterion confidence intervals
do not establish confidence in that cross-process comparison; inspect the
repeated samples and host activity before interpreting small differences.

The [2026-09-22 investigation](reports/2026-09-22-nar-generation/README.md)
compares the durable-generation fix with its parent and retains the exact
dependency lockfile and raw samples.

The [cache-hit follow-up](reports/2026-09-22-nar-cache-hit/README.md) defers
capturing the generation until measurement is needed. Complete cache hits avoid
that lookup. Partial hits reload their facts after generation capture, paying
one extra association read to prevent reuse of concurrently invalidated facts.
Cold misses and explicit scrubs retain their previous number of fact lookups.
The expanded corpus measures scrubs as well as cached reads and raw intake;
partial-hit enrichment is measured separately by `nar_partial`.

## Partial association enrichment

`nar_partial` adds canonical SHA-512 to a SHA-256-only association. Its four
cases cover local and memory repositories with 1 and 256 files of 1,024 bytes.
Each iteration creates a fresh repository, imports the fixture, verifies that
SHA-512 is absent, and warms the existing association before starting the timer.
Only the partial `ensure_nar` call is timed, including its measurement and merge.
Assertions, a subsequent complete cache hit, a stored-content scrub, and cleanup
are excluded. Independent SHA-256/SHA-512 digests, size, encoding-pass count and
payload-byte count gate every measurement. Fresh repositories prevent subsequent
iterations silently becoming complete cache hits. This measures warm enrichment
of newly imported data, not aging databases or contention. The one-file and
256-file cases bound traversal cost; they do not represent a discovered threshold.

```sh
cargo bench --bench nar_associations -- nar_partial --test
cargo bench --bench nar_associations -- nar_partial
python3 benchmarks/tools/compare_nar_generation.py \
  --case-set partial --baseline-binary /path/to/baseline-binary \
  --candidate-binary /path/to/candidate-binary \
  --repetitions 4 --output /tmp/nar-partial-comparison
```

The group is included in `benchmark all --suites core-primitives` through
`nar_associations` and registered as `nar-partial-hit` in the manifest.

The [partial-hit investigation](reports/2026-09-22-nar-partial/README.md) retains
the original noisy comparison batches and a September 23 follow-up with six
alternating pairs on a quieter host. The follow-up observes a 74 microsecond
(+2.9%) penalty for one-file local partial hits and no consistent slowdown for
256 files; the report retains the paired variation and measurement limitations.
