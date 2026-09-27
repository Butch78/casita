# Adaptive pack promotion: measured before/after, 2026-09-06

**Discarded.** The production changes and their tests were reverted. The
benchmark and historical measurements are retained. The modest serial gain,
increased traffic and tail latency, and coverage limited to 256 KiB packs did
not justify adding a timing heuristic to the default policy. The remote default
pack target is 16 MiB.

The experimental policy used observed response wait and body transfer cost to
guide whole-pack admission after cache pressure. It recovered throughput on
high-latency, fast connections while retaining range caching on the measured
slower connections. Cache capacity and the configured promotion threshold
were unchanged. All results below describe that discarded candidate.

## Repeated comparisons

Before is `947f2f69081b297e86bf03d237dbfe22a59c9344`, which already includes the
range cache. After is that revision plus the subsequently reverted adaptive admission changes.
These baselines differ from the earlier
[cache/network report](2026-09-06-cache-network.md).

All rows use shuffled reads after a complete sequential priming scan, 100 ms
RTT, and medians of three paired repetitions. The small fixture uses a 1 MiB
cache, 4 MiB working set and 128 reads; the large fixture uses 8 MiB, 32 MiB and
512 reads. Rate caps apply per TCP connection, in each direction.

| Working set | Concurrency | Rate | Before | After | Time change | Payload GETs |
|---|---:|---:|---:|---:|---:|---:|
| 32 MiB | 1 | 8 MiB/s | 53.896 s | 52.121 s | **−3.3%** | 484 → 425 |
| 4 MiB | 8 | Unlimited | 1.630 s | 1.125 s | **−31.0%** | 125 → 81 |
| 4 MiB | 8 | 8 MiB/s | 1.759 s | 1.457 s | **−17.1%** | 125 → 82 |
| 4 MiB | 8 | 1 MiB/s | 2.650 s | 2.653 s | +0.1% | 125 → 125 |
| 4 MiB | 1 | 8 MiB/s | 13.898 s | 13.993 s | +0.7% | 126 → 115 |

Negative time change means faster. The small serial case remains close to the
crossover: fewer requests do not guarantee lower elapsed time. These shared-host
measurements are exploratory, not statistical performance gates.

For the large case, median backend payload traffic rises from **30.25 to
63.56 MiB**, and p95 read latency rises from **116.49 to 134.86 ms**. Larger
individual reads cost more, while cache hits reduce total workload time. The
policy optimizes estimated elapsed time; it does not minimize traffic.

The large-case wall times were 53.896 / 53.903 / 53.317 seconds before and
53.702 / 52.121 / 51.512 seconds after, in repetition order. The numerical
receipt retains each repetition's traffic, requests and latency percentiles;
these can change with observed costs and concurrent completion order. Total
GET medians are calculated from each sample's total, not by summing separate
medians of range and whole-pack requests.

## Workload controls

The controls use the small fixture and one paired repetition each:

- At 0 and 25 ms RTT with 8 MiB/s, cold and warm shuffled reads retain the same
  backend GET counts at concurrency 1 and 8. Elapsed-time differences are below 2%.
- At 100 ms RTT and 1 MiB/s, serial shuffled reads take 20.906 → 20.875 seconds,
  with the same 126 range requests and bytes. The repeated concurrent case
  likewise retains its 125 range requests and bytes.
- All four sequential cases at 100 ms RTT, concurrency 1/8 and 1/8 MiB/s retain
  the same 64 GETs and payload bytes, with elapsed-time differences below 0.2%.
- Skewed reads at 1 MiB/s take 4.857 → 4.860 seconds serially and
  0.830 → 0.829 seconds concurrently. At 8 MiB/s they take 3.024 → 2.990 seconds
  serially and 0.552 → 0.514 seconds concurrently. These single pairs are controls,
  not evidence of a repeatable skewed-workload speedup.

All **128 final samples** pass byte-for-byte payload verification, phase checks,
and paired fixture/access-order/physical-layout identity checks. All **56 fitting
warm samples** fetch zero backend payload bytes and make zero payload GETs.
Coverage comprises 12 large-fixture, 36 concurrent, 12 small serial, 32 lower-RTT
cold/warm, 32 sequential/skewed and 4 slow-serial samples.

## Implementation

- Successful payload reads update a constant-size moving average of response
  wait and body transfer cost per byte. The first eight observations are
  averaged; later observations have weight 1/8. Failed or invalid reads do not
  update it.
- After eviction pressure, scattered misses can reach the configured promotion
  threshold when estimated extra transfer costs at most one third of response
  wait. At least four observations and a 1 ms average response wait are required.
  The estimate uses the pack's actual length.
- Under that condition, ranges can fill spare capacity but cannot evict
  resident data. Whole-pack promotions replace ranges under the same byte
  budget. Admission history is capped at 4,096 packs and accepts new packs
  after reaching the cap. Expensive transfers clear scattered-miss history
  and return admission to the existing sequential promotion policy.
- Cache-lock and whole-pack single-flight wait are excluded from observations.
  Range requests use the same `get_opts(...).bytes()` implementation as
  `ObjectStoreExt::get_range`, split to observe response and body timing.
  Length validation, integrity checking, invalidation and disabled-cache
  behavior are retained. The public API is unchanged; its documentation is updated.

The estimate is an admission heuristic. Response wait can include backend work,
retries and buffered body bytes; transfer time is not a precise measurement of
link bandwidth. Costs change admission only after pressure; fitting-cache
behavior remains eager. These measurements do not establish performance across
other pack sizes, production S3 services or changing network paths.

## Reproduction and receipts

Both binaries use the identical helper and Cargo.lock, built with
`--release --features s3,ssh,experimental --locked`. The before binary SHA-256
is `f3656577de5c6b95027d6885d9de095e4c52ea4c8edebc93f6401de52d40f626`;
the after binary is
`2e51bf08eda74fe0e192a337e6df984b0e84bab433a787f32533caab1ff047b4`.

```sh
benchmark run pack-cache-network --profile smoke --no-build \
  --helper /path/to/adaptive/pack_cache_network \
  --baseline-helper /path/to/947f2f6/pack_cache_network \
  --patterns random --phases warm --concurrency 8 \
  --rtt-ms 100 --bandwidths-kib 0,1024,8192 --repetitions 3 \
  --output benchmarks/results/cache-adaptive-concurrent.json
```

Use `--profile standard --reads 512 --concurrency 1 --bandwidths-kib 8192`
for the large case. The backend is RustFS on the shared Ryzen 7 7840S Linux
host. Fixtures use 64 KiB deterministic payloads and 256 KiB pack targets.
OS caches are not flushed. Setup and priming are untimed, wall time includes
byte verification, and per-read percentiles exclude verification and waiting
for admission. Builds ran outside measurements.

Raw JSON, logs, commands, source snapshots and binaries remain under
`benchmarks/results/2026-09-06-cache-adaptive/`. The initial 24-sample pilot is
retained separately and excluded from final comparisons: it lacks the change
that protects resident packs from range admission when transfers are cheap.

The [numerical receipt](2026-09-06-cache-adaptive.json) retains every repetition,
matrix result hashes, source/binary identities and environment metadata.

Validation: **484 all-feature library tests passed** (17 ignored), including
new cost-transition, byte-budget, threshold, pack-size and bounded-history
tests. **40 all-feature integration tests passed**. All-feature/all-target
Clippy with warnings denied, formatting and `git diff --check` passed.
The numerical receipt includes the test-log hashes.
