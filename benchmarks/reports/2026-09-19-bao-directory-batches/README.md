# Bao directory fsync batching experiment

Decision: do not adopt this prototype. It reduces directory fsync counts, but
this debug-profile investigation does not establish a reliable latency benefit.
No production changes from the prototype are retained. Obrador remains pinned
to Casita `9f44e1b`.

The prototype reuses LocalDurability's prepared-file publication. The pinned
object-store task prepares each Bao sidecar, then a shared gate drains up to 16
ready writes. Each file is synced before rename, and each distinct directory
ancestor is synced before returning success. No caller waits for a full batch.
Only default overwrite PUTs in the local `bao/b3/` namespace take this path.

This removes duplicate directory syncs but serializes directory syncing across
the batch and always syncs ancestors through the repository root. Those are
potential costs. Their individual effects were not isolated.

## Measurements

All cases use the permanent `nar_import` benchmark, a fresh persistent repository,
10 Criterion samples, and the same debug profile. Each sample checks SHA-256,
NAR size, payload bytes, no intake encoding pass, and a full stored-content scrub.
Shared-host load and filesystem caching were uncontrolled. Builds finished
before measurement. These are exploratory results, not release-speed claims.

Mean milliseconds reported by Criterion:

| Files × bytes | Baseline first | Prototype first | Baseline repeat | Prototype repeat |
| --- | ---: | ---: | ---: | ---: |
| 32 × 16385 | 249.21 | 480.36 | 130.84 | 157.75 |
| 32 × 65535 | 246.36 | 526.78 | 385.71 | 194.14 |
| 16 × 131073 | 324.64 | 236.20 | 340.37 | 391.44 |

The direction reverses for two cases on repetition. The first prototype run
stopped when Criterion reached a newly added 15-file case without a saved
baseline; the remaining shared cases were then run with an exact filter. No
result for that missing-baseline case is claimed. Raw first-run console output
and baseline/repeat sample arrays are retained in this directory.

One separate correctness-mode syscall trace of the 32 × 16385 case showed:

| Sync calls across the whole benchmark invocation | Baseline | Prototype |
| --- | ---: | ---: |
| Bao directories | 91 | 44 |
| Bao files | 32 | 32 |
| Pin ledger | 28 | 28 |
| All fsync/fdatasync calls | 189 | 147 |

The full traces include setup, import, and scrub. They do not isolate import
latency. SIGCHLD notifications from Criterion's Cargo metadata lookup are not
sync calls. Raw traces are retained; no successful sync calls were excluded.

## Correctness and limits

The prototype passed all 36 NAR tests, all 22 permanent benchmark correctness
cases, and a new grouped-write test covering 1/15/16/17 writes, multiple batches,
and failed renames. The new test is retained in the prototype patch. The
prototype was rejected before full crash testing or a release closure comparison;
those are required before reconsidering adoption. Passing process-death tests
would still not simulate power loss.

The production benchmark corpus retains 15/16/17-file sidecar cases. They run
through the existing `nar_import` registration in `benchmarks/manifest.json`
and `benchmark all`. No new runner is needed.

## Reproduction

In an isolated checkout of `9f44e1b`, enter the development shell. Preserve the
baseline debug benchmark executable, then apply `prototype.patch.gz` using
`gzip -dc .../prototype.patch.gz | git apply`. Rebuild and preserve the candidate.
`results.json` records the original binaries' hashes and raw Criterion data.

```sh
cargo test --locked --bench nar_import -- --test
# Preserve the executable path printed by Cargo as BEFORE or AFTER.
"$BEFORE" --bench 'nar_import/bytes-(16385/32|65535/32|131073/16)$' --save-baseline before
"$AFTER" --bench 'nar_import/bytes-(16385/32|65535/32|131073/16)$' --baseline before
strace -f -qq -ttt -T -yy -e trace=fsync,fdatasync -o before.strace \
  "$BEFORE" --test 'nar_import/bytes-16385/32$'
strace -f -qq -ttt -T -yy -e trace=fsync,fdatasync -o after.strace \
  "$AFTER" --test 'nar_import/bytes-16385/32$'
```

Prefer investigating sidecar packing next, since it could reduce file fsyncs as
well. A smaller alternative is a directory-sync coordinator that preserves
parallel flushing of independent directories, with explicit failure and crash
coverage. The rejected shared gate should not be reinstated without that work.

After restoring production code, all 22 benchmark correctness cases passed again.
Formatting and diff checks passed. CI was skipped.
