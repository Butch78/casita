# Hash inputs in committed source repositories

The [raw report](2026-09-11-hash-inputs.json) measures every committed regular
file occurrence in three local Git snapshots. These are cold payload-writer
workloads, not production traffic or incremental imports. Symlinks/submodules
are excluded and counted; duplicate contents remain separate file occurrences.
No working-tree files are read. Casita's snapshot includes retained benchmark
reports and assets, which materially affect its byte distribution.

| Corpus | Commit | Files | Logical bytes |
|---|---|---:|---:|
| Casita | c7a62d5 | 1,463 | 56,620,387 |
| devenv | 0bf6765ce7071d98ed137ecfe02d1e435007c971 | 1,471 | 13,094,639 |
| nixpkgs | 89ea88681d92c564085254528206f7587912f0c1 | 54,260 | 196,527,053 |

## Cumulative whole-file counts

Percent of regular-file occurrences at or below each threshold:

| Size | Casita | devenv | nixpkgs |
|---|---:|---:|---:|
| 64 B | 13.67% | 11.28% | 0.88% |
| 1 KiB | 28.50% | 59.08% | 44.54% |
| 16 KiB | 82.64% | 94.09% | 98.32% |
| 64 KiB | 93.16% | 98.03% | 99.60% |
| 256 KiB | 97.54% | 99.39% | 99.92% |
| 1 MiB | 99.25% | 99.80% | 99.99% |
| 4 MiB | 99.93% | 100.00% | 99.99% |

Count and byte shares tell different stories. Files above 64 KiB carry
84.00% of Casita's bytes, 67.94% of devenv's, and 43.37% of nixpkgs'. Tiny
files dominate counts, but these data alone do not establish where CPU time
is spent or rule out gains from large-file parallel hashing.

## Chunk work and digest reuse

FastCDC uses the current defaults: minimum 128 KiB, average 256 KiB, maximum
512 KiB. Every reference chunk sequence was checked against an actual cold
payload write, followed by complete readback. The populations below count
logical chunk occurrences before storage deduplication.

| Corpus | Storage chunks | Existing small-file reuse | Additional EOF reuse | Predicted independent chunk hashes |
|---|---:|---:|---:|---:|
| Casita | 1,372 | 1,207 | 34 | 131 |
| devenv | 1,482 | 1,450 | 6 | 26 |
| nixpkgs | 54,407 | 54,136 | 50 | 221 |

Empty files have a whole-file hash and no storage chunk. Existing reuse
covers nonempty files below the chunk minimum. The new EOF optimization
covers single-chunk files at or above the minimum and below the maximum;
at the maximum the reader cannot infer EOF from a full buffer. These reuse
counts are predictions from the verified chunk layout and writer rules,
not instrumented runtime call counts.

The new EOF reuse applies to 2.32%, 0.41%, and 0.092% of file occurrences,
respectively. Most files in these snapshots already use the older small-file
fast path. This bounds the reach of that particular optimization without
claiming a throughput improvement.

Repository-level verification, metadata hashes, validation/readback hashing,
and unchanged-file skip decisions are outside these histograms. A complete
production hash-call distribution still requires instrumentation of those
paths and a defined mix of fresh and incremental imports.

## Permanent corpus and validation

`benches/hash_inputs.rs` is registered in `benchmarks/manifest.json` and runs
through `benchmark all`, which retains `hash-inputs.json` for the bounded
default corpus. The default has 28 deterministic random/zero fixtures,
including minimum/maximum minus, exact, and plus one byte. Optional external
corpora are identified by their full Git revision in Criterion case names,
so different snapshots cannot silently share a benchmark identity.

Every tested file must match its independently computed BLAKE3 digest,
reference FastCDC chunk identities/sizes, and full byte-for-byte readback.
The four correctness-only corpus runs passed. No timing claim is based on
these scans. The JSON retains source and executable fingerprints.

```sh
devenv shell cargo bench --features experimental --bench hash_inputs --no-run
CASITA_HASH_REPOSITORY=/path/to/repo CASITA_HASH_REPORT=/tmp/hash-inputs.json \
  /path/from/cargo/hash_inputs-BUILD_ID --test
```

Omit `--test` to measure whole-file hashing and cold payload writes on the
same corpus. Setup and readback are excluded from write timings. The corpus
is held in memory and rejects more than 1 GiB of logical file data; this is
a bounded benchmark, not a streaming production importer.
