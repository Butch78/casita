# Historical catalog marking: scaling investigation

For reproduction after packed-reader integration, first reconstruct the
[historical source](2026-09-14-gc-source-reproduction.md), then use this report's commands.

All 24 optimized-build samples passed the exact retained-path union gate. The next focused change should deduplicate immutable shard traversal across held roots within one marking pass. These are baseline measurements taken before the optimization. See the [paired shared-shard comparison](2026-09-13-catalog-marking-dedup.md) for the subsequent implementation and validation.

At 65,536 entries, eight identical holds took a median 402 ms, versus 2,454 ms for eight distinct roots sharing the same base: 6.1× marking time for exactly the same 262,144 retained paths. Their median RSS increases were nearly equal (30.46 and 30.97 MiB). Identical roots are already deduplicated. Distinct roots re-decode and re-mark shared shards; the byte cache avoids most repeated object reads but not that work.

## Measurements

Three fresh-process samples per case, with cases interleaved by repetition. Timing includes pin inventory acquisition, catalog decoding, shard fetching and payload marking. Fixture setup and exact path-set comparison are outside the timer.

| Entries per base | Holds | Roots | Mark median ms (range) | Retained paths | Path strings MiB | RSS increase MiB, median | Catalog GETs |
|---:|---:|---|---:|---:|---:|---:|---:|
| 1,024 | 1 | identical | 4.17 (2.65–5.73) | 4,096 | 0.36 | 0.71 | 33 |
| 1,024 | 8 | identical | 4.87 (2.53–5.40) | 4,096 | 0.36 | 0.73 | 33 |
| 1,024 | 8 | overlap | 28.27 (24.95–31.98) | 4,096 | 0.36 | 0.73 | 40 |
| 1,024 | 8 | disjoint | 46.46 (34.78–64.36) | 32,768 | 2.84 | 5.34 | 264 |
| 65,536 | 1 | identical | 331.98 (302.68–384.10) | 262,144 | 22.75 | 30.52 | 33 |
| 65,536 | 8 | identical | 402.30 (325.82–435.84) | 262,144 | 22.75 | 30.46 | 33 |
| 65,536 | 8 | overlap | 2454.27 (2282.99–2782.14) | 262,144 | 22.75 | 30.97 | 40 |
| 65,536 | 8 | disjoint | 3576.98 (3496.75–4044.46) | 2,097,152 | 182.00 | 292.32 | 264 |

## Interpretation and limits

- Identical roots: one root map plus 32 payload shards, regardless of hold count. The timing difference between one and eight identical holds is noisy; this does not establish a regression or certify a 20% ceiling.
- Overlapping roots: eight root maps plus the same 32 cached shards. The same 32 shards are nevertheless decoded and their paths inserted eight times. Deduplicating this traversal is the preferred next experiment. Preserve complete reference validation and test distinct per-root additions; a digest-only shortcut must not bypass conflicting metadata checks.
- Disjoint roots: eight genuinely different bases require 264 catalog GETs and eight times the retained paths. At the larger size, path characters alone occupy 182 MiB and observed RSS grows about 292 MiB. Shared-shard deduplication will not remove this memory growth. A compact retained-path representation is another option to measure later; this data does not yet justify the disk I/O and complexity of spilling.
- The fixture uses one synthetic pack, chunk and blob per entry, with valid catalog metadata but no payload files. It holds full snapshots through the memory pin ledger and uses an in-memory object store. It isolates historical marking, not real payload throughput, durable ledger cost, online hold admission latency or whole-GC duration.
- Each root uses a 4-bit sharded base without run overlays. Overlapping roots differ only in generation and share every shard; disjoint roots share no payload identities. Partially shared bases, large run chains, checkpoint/inline roots and catalogs exceeding the shard cache remain unmeasured. No performance threshold was located by this bounded matrix.
- RSS is sampled immediately before and after marking, with the mark alive. It is not peak heap or an exact allocator measurement: fixture allocations, the independent expected-path set, allocator reuse and cached catalog bytes influence it. Path-string totals exclude hash-table slots and allocation overhead.
- This shared host had unrelated compilation and benchmark activity. Our builds and tests finished before release measurements. Treat medians as exploratory; exact retained-path and GET counts are stronger evidence. No before/after optimization speedup is claimed.

## Reproduce

Runtime baseline: `62ea96d5f167a03233a50c0a97773d0f25ea4ce1`, with the benchmark instrumentation in this change. The raw report retains compiler/host metadata, binary SHA-256, invocation-time source hashes, configuration and all process output.

```console
cargo test --locked --offline --release --features cli --lib --no-run -j 2
benchmark run catalog-marking --profile standard --repetitions 3 --probe-binary /absolute/path/to/casita-lib-test --no-build --output results.json
benchmark all --suites catalog-marking --repetitions 1 --bin-dir /absolute/path/to/bin --output /tmp/catalog-marking-smoke
```

The `bin` directory must contain the library test executable named `casita-lib-test`. This run built through the cached devenv shell with `CARGO_BUILD_BUILD_DIR=/tmp/casita-online-holds-build` and Cargo config `build.build-dir="/tmp/casita-online-holds-build"`.

Validation: all 24 release probes passed; the four-case smoke suite passed through `benchmark all` using the all-feature test build; 70 existing pack tests passed; 239 Python tests passed; all-feature/all-target Clippy with warnings denied passed; formatting and diff checks passed.

[Raw release measurements](2026-09-13-catalog-marking.json)
