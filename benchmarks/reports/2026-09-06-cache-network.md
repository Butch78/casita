# Cache pressure over S3: measured before/after, 2026-09-06

The range-cache change reduces traffic, but can increase total read time on
high-latency connections. The new `pack-cache-network` benchmark measures that
tradeoff using real RustFS requests through a calibrated TCP delay/rate proxy.

## Repeated comparisons

All rows below use shuffled reads, a cache primed by one complete sequential
scan, 100 ms RTT, and medians from three before/after repetitions. Bandwidth is
per TCP connection in each direction; unlimited means no configured rate cap.

| Cache / working set | Reads | Concurrency | Bandwidth | Before | After | Change |
|---|---:|---:|---:|---:|---:|---:|
| 8 MiB / 32 MiB | 512 | 1 | 8 MiB/s | 51.618 s | 53.540 s | **3.7% slower** |
| 1 MiB / 4 MiB | 128 | 1 | 8 MiB/s | 13.981 s | 13.904 s | Approximately unchanged |
| 1 MiB / 4 MiB | 128 | 8 | 8 MiB/s | 1.466 s | 1.764 s | **20.4% slower** |
| 1 MiB / 4 MiB | 128 | 8 | Unlimited | 1.232 s | 1.643 s | **33.3% slower** |

At the original working-set sizes, the traffic and latency measurements explain
why one metric alone is insufficient:

| Metric, 512 serial reads | Before | After |
|---|---:|---:|
| Backend payload GETs | 425 | 484 |
| Backend payload bytes | 63.56 MiB | 30.25 MiB |
| Whole-pack GETs | 197 | 0 |
| Cache hits | 87 | 28 |
| p95 read latency | 134.69 ms | 111.61 ms |

Bytes fetched decrease **52.4%**, and p95 read latency improves **17.1%**.
However, the new policy loses some reuse supplied by whole-pack reads: cache
hits decrease and requests increase **13.9%**. Extra round trips outweigh the
saved transfer time in this case. This makes total workload time worse even
while the slower individual reads get faster.

The before wall times were 51.392 / 51.903 / 51.618 seconds; after times were
55.093 / 53.434 / 53.540 seconds. These are exploratory measurements on a shared
host, not statistical performance gates. Concurrent request completion order
can change cache hits and request counts; the receipt retains those values and
every repetition's wall time.

## Where the tradeoff helps

The broader sweep uses the 1 MiB cache / 4 MiB working set and 128 reads.
These selected results have **one paired repetition each**:

| Access / concurrency | RTT | Bandwidth | Before | After | Change |
|---|---:|---:|---:|---:|---:|
| Shuffled / 1 | 0 ms | 8 MiB/s | 2.414 s | 1.204 s | 50.1% faster |
| Shuffled / 1 | 25 ms | 8 MiB/s | 5.355 s | 4.380 s | 18.2% faster |
| Shuffled / 1 | 100 ms | 1 MiB/s | 29.542 s | 20.868 s | 29.4% faster |
| Shuffled / 8 | 100 ms | 1 MiB/s | 3.305 s | 2.645 s | 20.0% faster |
| Skewed / 1 | 100 ms | 1 MiB/s | 6.095 s | 4.960 s | 18.6% faster |
| Skewed / 8 | 100 ms | 1 MiB/s | 1.418 s | 0.832 s | 41.3% faster |

Sequential reads retain the same 64 backend requests and payload bytes in all
six high-RTT bandwidth/concurrency combinations. Their measured time differences
are within 0.5%. All **68 paired-result warm cases below cache capacity** fetch
zero backend payload bytes and make zero backend payload GETs.

The result supports making promotion account for request latency, bandwidth and
reuse across nearby chunks in a pack. Simply minimizing fetched bytes misses
the high-RTT regressions; universally fetching whole packs would give up the
low-bandwidth gains. The next optimization should use these cases to evaluate
when to promote scattered range reads under pressure, with bounded per-pack
history and the existing cache byte budget.

## Benchmark implementation and coverage

- `examples/pack_cache_network.rs` creates deterministic 64 KiB payloads in
  256 KiB target packs. Setup writes directly to RustFS. Measured reads go only
  through the proxy. Every returned payload is compared byte-for-byte.
- Each phase opens a fresh Casita handle. Cold reads start immediately; warm
  reads follow a full sequential priming scan. Priming and setup are untimed.
  Working sets must straddle the actual physical cache capacity.
- The driver alternates before/after ordering for each case. It verifies
  payload, access-order and physical-layout identities across each pair, records
  executable hashes, and rejects missing phases or invalid measurements.
- Results include wall time, p50/p95/p99, payload bytes and GETs, cache hits,
  promotions, evictions, peak concurrent logical reads and process RSS. Raw
  process output and bounded RustFS log tails survive failures. Dashboard
  grouping separates concurrency and before/after variants.
- The entrypoint is registered with `benchmark all` and `benchmark revisions`.
  Historical revisions lacking the helper need the identical harness source
  added before building; that addition is part of this experiment's provenance.

The completed measurement set contains **160 paired-comparison samples**:
12 small-fixture focused samples, 12 original-size samples, 72 high-RTT sweep
samples, 16 lower-RTT controls, 24 concurrent confirmation samples and 24
cold-cache controls. An additional 24-sample unshaped smoke run checks both
phases and all access patterns/concurrency settings on the new binary; its
timings were excluded because the baseline was still compiling.

This is a focused subset of the configurable matrix. In particular, the
original-size measurements use **512 reads after fresh sequential priming**;
the earlier local-cache receipt used 8,192 reads and a different warmup history.
The earlier counterfactual time estimate must not be compared directly with
these shorter measurements. This benchmark does not measure OpenSSH, RPC,
production S3 service behavior or a shared aggregate bandwidth cap. OS caches
are not flushed. Wall time includes byte verification; per-read percentiles
exclude verification and waiting for admission. Peak in-flight counts describe
logical reads, not individual backend requests. Process RSS includes setup.

## Reproduction and receipts

The before library is `69077967b69914d9f7f0096a293c4c7f09ac05cb`, and the after
library is `947f2f69081b297e86bf03d237dbfe22a59c9344`. Both use the identical new
helper and Cargo.lock, Rust 1.96.0, and release features `s3,ssh,experimental`.
The baseline crate was explicitly rebuilt after switching source trees in the
shared build directory. Binary hashes differ and are retained with the results.
The backend is Linux RustFS 1.0.0-rc.1 on the shared Ryzen 7 7840S host.

Proxy calibration measured a 64 KiB response at 108.94 ms and a 256 KiB response
at 131.92 ms with 100 ms RTT and 8 MiB/s configured, consistent with the
intended propagation delay plus transfer time. The calibration checked exact
response bytes and is included in the receipt.

```sh
benchmark run pack-cache-network --profile standard --no-build \
  --helper /path/to/after/pack_cache_network \
  --baseline-helper /path/to/before/pack_cache_network \
  --reads 512 --patterns random --phases warm --concurrency 1 \
  --rtt-ms 100 --bandwidths-kib 8192 --repetitions 3 \
  --output benchmarks/results/cache-network-original-size.json
```

Use the smoke profile with `--concurrency 8 --bandwidths-kib 0,8192` to reproduce
the repeated concurrent cases. The [numerical receipt](2026-09-06-cache-network.json)
contains medians, repetition values, input hashes, source/binary identities,
environment metadata and calibration measurements. Full JSON results, commands,
logs, source snapshots and binaries remain in the ignored directory
`benchmarks/results/2026-09-06-cache-network/`.

Validation: both release helpers built successfully; all 184 samples passed
payload and result validation; 142 Python tests passed; all-feature/all-target
Clippy, formatting and `git diff --check` passed.
