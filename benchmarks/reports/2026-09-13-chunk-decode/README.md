# Reuse the bounded chunk decoder

Chunk reads now reuse `compression::decompress`, removing the duplicate
streaming decoder in `blob/chunked/manifest.rs`. Sized single frames use the
existing cached bulk decoder; unsized and concatenated frames retain the
bounded streaming fallback. Chunk size and digest verification remain in
their existing callers.

This implements the candidate identified by the
[tar CPU profiles](../2026-09-13-tar-profile/README.md). Correctness and lower
output-buffer capacity are verified. The initial timing attempts failed host
admission. A later retry produced one accepted decoder matrix with promising
results, but its repeat exceeded the CPU limit; a repeatable speedup and
end-to-end tar improvement remain unverified.

## Capacity and correctness

The permanent `optimization/chunk_decompression` matrix has 108 cases:
nine sizes, deterministic random/text data, sized/unsized/concatenated frames,
and streaming/reused decoders. It includes both sides of 64 KiB and 128 KiB
and runs in `benchmark all`. Every timed iteration checks complete decoded
bytes outside the timer. Preflight also rejects undersized caps and truncated
frames.

Sized random-frame output Vec capacities from the correctness run:

| Decoded bytes | Former streaming decoder | Shared decoder |
|---:|---:|---:|
| 1,024 | 2,048 | 1,024 |
| 65,535 | 65,536 | 65,535 |
| 65,536 | 131,072 | 65,536 |
| 65,537 | 131,072 | 65,537 |
| 131,071 | 131,072 | 131,071 |
| 131,072 | 262,144 | 131,072 |
| 131,073 | 262,144 | 131,073 |
| 262,144 | 524,288 | 262,144 |
| 524,288 | 1,048,576 | 524,288 |

These are output Vec capacities, not total allocated memory or peak RSS.
The complete preflight records, including legacy fallback cases, are in the
[receipt](report.json).

Local validation passed: 75 chunk tests, 39 archive tests, all 108 decoder
benchmark cases and 16 tar benchmark cases in correctness mode, the Python
harness's 232 tests (two skipped), formatting, and Clippy with all features,
all targets, and warnings denied. Added unit coverage includes empty output,
legacy frames, exact/undersized caps, truncation, invalid trailing data, and
successful decoding after an error.

## Timing admission

Both runners required ten continuous seconds at or below 30% sampled
background CPU, with a 60-second admission deadline. Competing builds were
recorded without a separate veto. Neither attempt reached admission:

| Attempt | Median background CPU | Peak background CPU | Outcome |
|---|---:|---:|---|
| Decoder comparison | 36.83% | 52.83% | No timing cases ran |
| Paired tar comparison | 42.74% | 90.28% | No timing cases ran |

The receipt retains all 58 admission samples from each attempt. No other
processes were paused. The pending timing work is a two-run decoder comparison
with reversed variant order, followed by two matched baseline/candidate tar
pairs with reversed executable and concurrency order.

A subsequent [decoder timing recheck](recheck.json) also failed admission:
58 samples had median background CPU of 36.75% and a peak of 57.20%.
An initial three-sample check was below 30%, but the load did not remain
within the limit for the required ten continuous seconds. No timing cases
ran. Source and executable hashes still matched the validation provenance.

## Latest retry

The [next decoder attempt](decoder-retry-2.json) completed all 108 correctness
cases twice. The first matrix was accepted with median/peak background CPU
of 4.87%/19.89%. The reversed-order matrix reached 49.37% and was rejected.
The runner correctly withheld its paired comparison. Sources and immutable
executables were checked against the original validation provenance again.

For orientation, these are results from the **single accepted matrix only**:

| Sized random chunk | Streaming decode | Reused decode | Observed speed ratio |
|---:|---:|---:|---:|
| 1 KiB | 2.18 µs | 1.52 µs | 1.44× |
| 64 KiB | 7.22 µs | 2.53 µs | 2.85× |
| 256 KiB | 26.01 µs | 5.86 µs | 4.44× |
| 512 KiB | 54.44 µs | 12.90 µs | 4.22× |

Across the 18 sized-frame cases, the median reused/streaming time ratio was
0.618. The unsized and concatenated controls had median ratios of 1.005 and
1.007. These are descriptive single-run results, not a completed repeated
comparison or evidence of end-to-end tar speedup. No rejected measurements
are included in these figures.

The [tar retry](tar-retry-2.json) failed admission before running any cases:
median background CPU was 25.57%, but peaks of 52.74% prevented the required
ten continuous seconds within the limit. Raw outputs are in
`benchmarks/results/2026-09-13-chunk-decode/{decoder-retry-2,tar-retry-2}/`;
the linked receipts retain estimates, samples, correctness preflights, commands,
binary hashes, and host activity where available.

## Reproduction and provenance

The receipt includes source and executable hashes, fresh Cargo artifact
records, the production patch, build and validation logs, and both admission
reports. Comparison against the saved tar baseline found that only
`src/blob/chunked/manifest.rs` changed among its recorded production and
benchmark sources; Cargo.toml and Cargo.lock matched. Immutable executables
and raw logs remain under `benchmarks/results/2026-09-13-chunk-decode/`.

Build and validate the permanent cases:

```sh
cargo test --features experimental --lib blob::chunked
cargo test --features experimental --lib tar::
cargo bench --features experimental --bench optimization --bench tar_import --no-run
cargo bench --features experimental --bench optimization -- --test chunk_decompression
cargo bench --features experimental --bench tar_import -- --test
cargo clippy --all-features --all-targets -- -D warnings
cargo fmt --all -- --check
python3 -m unittest discover -s benchmarks -p 'test_*.py'
```

For timing, copy the built executables to immutable paths and use fresh output
directories. The former decoder is retained inside the optimization binary,
so its comparison uses one executable. The tar comparison requires a saved
baseline built before the production change:

```sh
python3 benchmarks/reports/2026-09-13-chunk-decode/run.py \
  /path/to/optimization /tmp/chunk-decode-new
python3 -m benchmarks.tar_compare \
  --baseline-binary /path/to/tar-baseline --binary /path/to/tar-candidate \
  --output /tmp/chunk-decode-tar-new --repetitions 2 \
  --warm-up-time 0.2 --measurement-time 1 --quiet-timeout 60 \
  --max-external-cpu-percent 30 --allow-competing-builds
```

Recreate the retained receipt from the raw investigation directory:

```sh
python3 benchmarks/reports/2026-09-13-chunk-decode/summarize.py \
  benchmarks/results/2026-09-13-chunk-decode /tmp/chunk-decode-report.json
```
