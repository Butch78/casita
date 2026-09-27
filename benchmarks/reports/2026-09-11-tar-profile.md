# Scoped CPU profiles of tar imports

Hashing has a substantially larger share of sampled user CPU for large files
than for tiny files. Six scoped profiles completed with the permanent tar
benchmark's correctness gates and zero reported lost samples. This provides
CPU attribution under contention; it does not provide a tar throughput
comparison or an isolated-host performance baseline.

| Archive | In-flight files | BLAKE3 | Compression symbols | Memory operations | Samples |
|---|---:|---:|---:|---:|---:|
| 256 × 1 KiB | 1 | 5.45% | 15.01% | 17.97% | 1,453 |
| 256 × 1 KiB | 16 | 6.57% | 11.04% | 22.76% | 1,199 |
| 4 × 4 MiB | 1 | 35.15% | 15.13% | 29.63% | 1,303 |
| 4 × 4 MiB | 16 | 34.62% | 16.16% | 28.70% | 1,895 |
| 32 × 1 KiB + 4 × 4 MiB | 1 | 31.49% | 16.40% | 29.38% | 1,872 |
| 32 × 1 KiB + 4 × 4 MiB | 16 | 32.25% | 17.92% | 31.52% | 2,560 |

Percentages are self cost in sampled user CPU events, across the importer
and its worker threads. Compression groups names containing ZSTD/HUF/FSE;
memory operations group memcpy/memmove/memset/memcmp. Inlined code, other
compression helpers, kernel CPU, and time spent waiting are not fully
attributed by these categories. The groups are diagnostics rather than an
exhaustive partition of import phases. Their percentages do not bound
wall-clock speedup in a concurrent pipeline.

These are the existing deterministic random-byte fixtures with an in-memory
object store and metadata store. They do not reproduce every real source
repository's content, compressibility, or storage latency. The earlier
[committed-repository histogram](2026-09-11-hash-inputs.md) describes a
separate population.

## Scope and evidence

`benches/bench_util/perf.rs` provides optional acknowledged perf FIFO control.
The tar benchmark enables events immediately before `request.import` and
disables them immediately afterward. Importer verification and publication
remain in scope. Fixture/repository creation, independent expected-root
construction, and benchmark readback checks remain outside. All root, file
count, byte count, publication, and full-readback checks still execute after
each import. With no profiling environment variables the control is absent.

`benchmarks/tar_profile.py` runs the existing small-256, large, and mixed
cases at limits 1 and 16, using `cpu-clock:u` at 499 Hz, inherited worker
sampling, DWARF call-chain capture, and Criterion's 10-second profiling mode.
It checks executable fingerprints, requires the benchmark's scope marker,
rejects empty/undersampled reports, records external host activity, and
retains failed results. These cases remain registered and run through
`benchmark all`; the profile runner is registered alongside the tar case.

The [JSON report](2026-09-11-tar-profile.json) retains commands, source and
binary fingerprints, per-case sample counts, symbol summaries, host activity,
and hashes of the raw perf recordings. The [flat reports](2026-09-11-tar-profile/large-16.perf-flat.txt)
retain symbols and thread names; all six flat reports are in the same report
directory. Raw recordings remain under `target/tar-profile-2026-09-11`.

Median external CPU utilization ranged from 37% to 87% across cases. This
contention, profiler overhead, and one run per case preclude a causal speedup
claim. The resumed guarded timing attempt was stopped during its quiet wait
so profiling could proceed; no timing result from it was accepted.

## What to investigate next

Tiny-file hash batching is not the first target suggested by these profiles:
hashing accounts for roughly 5–7% of sampled user CPU, while memory operations,
compression, allocation, and scheduling are more prominent in aggregate.
A next tiny-file experiment should attribute the copies/allocations before
changing them and should include realistic source content.

For the four-large-file case at limit 16, BLAKE3 accounts for 19.05% of total
sampled user CPU on the importer thread and 15.57% on Tokio worker threads.
The worker component already has concurrency. Whole-file streaming hashing
and verification are candidates for closer inspection, but this profile
cannot determine their individual shares.

The [hash caller report](2026-09-11-tar-profile/large-16.hash-callers.txt)
contains invalid caller addresses above the AVX-512 assembly routine. That
unwinding limitation prevents reliable attribution to exact hash call sites.
Do not infer that all importer-thread hashing is the streaming file digest
or remove repository verification to reduce the observed cost.

## Reproduction and checks

```sh
devenv shell cargo bench --features experimental --bench tar_import --no-run
python3 -m benchmarks.tar_profile \
  --binary /path/from/cargo/tar_import-BUILD_ID \
  --perf /path/to/perf --output /tmp/tar-profile-new --seconds 10
```

Six profiled cases passed their import/readback gates. Both normal tar orders
also pass all 16 cases in correctness-only mode. All-features/all-targets
Clippy, formatting, and 22 benchmark-harness/parser tests passed. Production
hashing and verification behavior is unchanged by this profiling work.
