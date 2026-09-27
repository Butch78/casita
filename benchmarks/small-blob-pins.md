# Small-blob pin protection

Obrador's batched NAR imports exposed a remaining cost: every nonempty small
file made two sequential durable pin updates, one for the blob and one for its
single chunk. At EOF the small-file branch already knows both identities.

The change protects the blob and chunk together before upload. The existing
chunk-admission check then finds its identity in the confirmed protection set.
This preserves protection-before-I/O and deletion-claim arbitration. Empty files,
the general chunker, and physical pack/path protection keep their existing paths.

## EOF-proven single chunks (2026-09-20)

The streaming chunker now coadmits the blob, chunk, and any Bao root path when
its first emitted chunk covers the entire EOF-confirmed input. The completed
hash carries the outboard length, so the writer uses the same resource set as
the small-file branch. Admission still completes before any payload I/O.
Subsequent checks reuse the confirmed protection set. No timer or delayed
sync is introduced.

The permanent suite now includes 2,047, 2,048, and 2,049 bytes at an average chunk
size of 1,024 bytes. Below the maximum, EOF can be known when the first chunk
is emitted. At the maximum the chunker emits before observing EOF, so separate
admissions remain. Data-dependent earlier cuts also retain separate admission.

| Bytes/file | Ledger edits before (16 files) | After |
| ---: | ---: | ---: |
| 64 | 16 | 16 |
| 511 | 16 | 16 |
| 512 | 32 | 16 |
| 513 | 32 | 16 |
| 2,047 | 32 | 16 |
| 2,048 | 32 | 32 |
| 2,049 | 49 | 49 |

These are exact revision advances, not syscall counts. The retained debug
binaries are suitable for correctness and edit comparisons, not throughput
claims. Every sample checks publication/readback, duplicate identity without
new edits, and release of all pins. The after suite also passed through
`benchmark all --suites small-blob-pins`.

Baseline: `e547cc9`. Raw samples and binary identities:
[`reports/2026-09-20-single-chunk-admission`](reports/2026-09-20-single-chunk-admission).
Use the reproduction command below with preserved `--probe-binary` and
`--no-build`, or allow the suite to build an optimized probe.

Regression coverage includes the small-file and chunker boundaries, exact
outboard-length propagation, repeated writes, and chunk/blob/Bao deletion
claims with both release and cancellation. The process-death publication
fixture includes a file above the small-file cutoff that still has one chunk.
All 74 chunked-store tests, five hashing-reader tests, the final process-death
publication matrix, formatting, and all-features/all-targets Clippy passed.

## Reproduction

The permanent `small-blob-pins` suite is registered in the manifest, revision
runner, and `benchmark all`. It uses a fresh local packed payload store and a
real durable pin ledger for each sample. The average chunk size is 1,024 bytes,
so 511, 512, and 513 bytes cover both sides of the small-file cutoff. Unique
payloads avoid accidentally benchmarking only the confirmed-pin cache.

```sh
python3 -m benchmarks.cli run small-blob-pins --profile smoke \
  --repetitions 1 --output /tmp/small-blob-pins.json \
  --report /tmp/small-blob-pins.md
```

Use `--probe-binary PATH --no-build` to compare retained test binaries. Run in
the development shell. The suite normally builds an optimized probe; explicitly
supplied debug probes are suitable for correctness and ledger-edit comparisons,
not release-throughput claims. It retains binary hashes, configuration, host
metadata, stdout/stderr, and individual samples. Publication, duplicate writes,
readback, and pin-release gates are outside the measured staging interval.
Ledger edits are pin-inventory revision advances, not filesystem sync counts.

The regression tests require one ledger edit for nonempty files below the
cutoff, check both identities, and verify that repeated writes add no edits.
A deletion claim on the chunk blocks the combined admission; cancellation must
release all writer pins after the outstanding operation settles.

## Native results

Retained debug probes built from `0d6015a` with the same regression/benchmark
code, before and after the writer change. The regression failed before the fix
(observed two edits, expected one) and passed afterward.

| Files | Bytes/file | Edits before | Edits after | Seconds before | Seconds after |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 64 | 64 | 128 | 64 | 1.099 | 1.226 |
| 64 | 511 | 128 | 64 | 1.094 | 1.099 |
| 64 | 512 | 128 | 128 | 1.302 | 1.973 |
| 64 | 513 | 128 | 128 | 1.087 | 1.560 |

The expected ledger-edit reduction is exact. The debug timing samples were
collected on a shared host alongside builds and do not establish a speedup.
The 512/513-byte controls retain their previous two admissions per file.

Artifacts: `/tmp/casita-small-pins-before.json`,
`/tmp/casita-small-pins-after.json`, and `/tmp/casita-small-pins-all`.
The last directory retains a successful `benchmark all --suites small-blob-pins`
run with the fixed binary. Both probe identities are recorded in the JSON:

- Before: `f7d6089e4238eedc6f0f4b74048132dc1747e6dae166d9978725b53f10ba66a7`.
- After: `ddfe814e4138cc7713d6abacb96d3710dbcc785e8d4623ca98b8ddc092db2bbf`.

## Obrador release-plugin validation

Obrador `45a2af1` was built with a temporary Cargo patch pointing to this Casita
worktree and its `casita-fs` package. Obrador's committed dependency pin and lock
file were not changed. The benchmark remains Obrador's permanent
`benchmarks/nar-publications.py --strace --files 1 64 256` fixture.

| Files | Pin-ledger fsyncs before | Pin-ledger fsyncs after | Seconds before | Seconds after |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 50 | 45 | 0.967 | 1.409 |
| 64 | 176 | 107 | 1.768 | 2.216 |
| 256 | 561 | 301 | 3.899 | 4.609 |

Counts cover the entire copy, including startup, directory payloads, and final
publication. All exported NARs match and Nix store verification passes. The
reduction is about 46% for the 256-file import. These shared-host timings do not
establish a latency improvement.

Before artifacts: `/tmp/obrador-nar-staging-syncs`.
After artifacts: `/tmp/obrador-nar-small-pins-fixed`.
The fixed plugin's SHA-256 is `4b9caab12084c8452c665a9b0070c25cb4ca945a922c02ec42b9d4133b0c9916`.

Validation passed: 73 chunked-store tests (one benchmark ignored), 84 repository
tests, 23 benchmark-harness tests, the new suite through `benchmark all`, scoped
Rust formatting, and `cargo clippy --all-targets -- -D warnings`. The patched
Obrador release build passed dynamic-selector and store-copy functional tests.

### Alternating release samples

After our builds and functional checks finished, four fresh 256-file imports
ran in the order before, after, after, before. All passed both content checks.
The host was still shared with other work.

| Order | Plugin | Pin-ledger fsyncs | Copy seconds | Blob staging seconds | Store open seconds |
| ---: | --- | ---: | ---: | ---: | ---: |
| 1 | before | 562 | 11.832 | 8.174 | 1.572 |
| 2 | after | 302 | 13.008 | 6.607 | 3.383 |
| 3 | after | 303 | 13.347 | 7.296 | 2.899 |
| 4 | before | 561 | 10.703 | 6.522 | 2.420 |

The fixed samples were slower end to end in this repeat. Startup and per-sync
costs varied substantially; these measurements do not demonstrate a latency
win. The demonstrated result is fewer durable ledger writes with the same
verified content and retention semantics. Do not turn the write-count reduction
into a throughput claim. Artifacts are `/tmp/obrador-small-pins-paired-{before,after}-{1,2}`.

Reproduce the alternating sequence in the Obrador development shell after
retaining the baseline and patched release plugins at the indicated paths:

```sh
export OBRADOR_NIX="$(command -v obrador-nix)"
for run in before-1 after-1 after-2 before-2; do
  export OBRADOR_PLUGIN="/tmp/obrador-small-pins-${run%-*}.so"
  python3 benchmarks/nar-publications.py "/tmp/obrador-small-pins-paired-$run" \
    --strace --files 256 || exit
done
```
