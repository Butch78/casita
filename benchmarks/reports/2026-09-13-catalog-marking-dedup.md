# Shared-shard payload marking: paired comparison

For reproduction after packed-reader integration, first reconstruct the
[historical source](2026-09-14-gc-source-reproduction.md), then use this report's commands.

Keep the change. At 65,536 entries, eight overlapping held roots now mark in a median 380 ms versus 2,835 ms before: **7.5× faster (87% less marking time)** with exactly the same 262,144 retained paths and 40 catalog GETs. At 1,024 entries the ratio of medians is 6.0×. Every paired overlapping case improved at both sizes.

The largest control-case slowdown by ratio of medians was 8.8%; the largest median of individual paired control ratios was 11.1%. Both are below the 20% target. However, unrelated host activity produced large timing excursions, so this experiment does **not certify a strict 20% overhead ceiling** or establish small control-case wins. Whole-GC latency remains unmeasured.

## Change and safety

- Each payload marking pass owns separate sets of processed pack-shard and manifest-shard references. References enter these sets only after successful decoding and marking. Their full metadata participates in identity, including prefix, entry count, encoded length and routing reference; payload table kinds stay separate.
- Root decoding and marking of inline additions and run overlays continue for every distinct root. Partially shared bases therefore retain their distinct shards and root-specific additions. No persistent cache, public API change, new spill path or narrower retention policy is introduced.
- Marking also checks the encoded length of cached shard bytes. A conflicting length can no longer pass merely because the shard byte cache already contains that digest. Existing pack entry-count and pack/manifest prefix validation remains active for differing references.
- The regression test covers partially shared bases, unique inline additions, a fresh second marking pass, and conflicting pack prefixes/counts/lengths and manifest prefixes/lengths after valid references and bytes have already been cached.
- Failure still aborts marking before physical deletion. Existing cancellation, hold and recovery tests pass.

## Results

Six repetitions per configuration and binary, 96 successful samples total. Binaries alternate execution order each repetition. All samples run in fresh processes and gate the exact retained-path union. Times cover pin inventory acquisition, root decoding, shard loading and path marking; setup and correctness checks are excluded.

| Entries/base | Holds | Roots | Before ms, median (range) | After ms, median (range) | Change in median | Median paired change |
|---:|---:|---|---:|---:|---:|---:|
| 1,024 | 1 | identical | 5.36 (2.82–12.50) | 4.17 (2.48–54.33) | -22.2% | -12.5% |
| 1,024 | 8 | identical | 4.80 (2.43–18.57) | 3.76 (2.46–10.92) | -21.6% | -14.7% |
| 1,024 | 8 | overlap | 24.97 (13.98–103.74) | 4.17 (2.70–23.36) | -83.3% | -80.7% |
| 1,024 | 8 | disjoint | 41.36 (30.98–239.60) | 44.98 (30.23–328.75) | +8.7% | +11.2% |
| 65,536 | 1 | identical | 410.16 (275.96–1146.34) | 401.48 (282.49–799.93) | -2.1% | +4.0% |
| 65,536 | 8 | identical | 393.26 (312.45–1040.19) | 425.72 (274.29–1219.23) | +8.3% | +2.6% |
| 65,536 | 8 | overlap | 2834.79 (1741.35–5995.91) | 379.92 (264.42–903.34) | -86.6% | -85.2% |
| 65,536 | 8 | disjoint | 4121.79 (2958.50–5416.69) | 3726.05 (2693.44–5200.83) | -9.6% | +1.2% |

Negative percentages mean faster. “Change in median” compares the two timing medians; “median paired change” first divides candidate by baseline within each repetition, then takes the median. The first method gives the headline 7.5× result. The second gives about 6.9× for the large overlap case.

Individual controls swung substantially: for example, the 1,024-entry single-hold case ranged from 2.48 to 54.33 ms in the candidate. The small disjoint case had one candidate/baseline pair at 2.26× despite a much smaller median change. Alternation reduces time-order bias but does not remove host contention. All twelve overlap pairs improved, with candidate/baseline ratios between roughly 0.08 and 0.25.

## Memory and limits

| Entries/base | Holds | Roots | Retained paths | Before RSS increase MiB, median | After RSS increase MiB, median |
|---:|---:|---|---:|---:|---:|
| 1,024 | 1 | identical | 4,096 | 0.70 | 0.85 |
| 1,024 | 8 | identical | 4,096 | 0.66 | 0.87 |
| 1,024 | 8 | overlap | 4,096 | 0.68 | 0.86 |
| 1,024 | 8 | disjoint | 32,768 | 5.26 | 5.48 |
| 65,536 | 1 | identical | 262,144 | 30.77 | 30.65 |
| 65,536 | 8 | identical | 262,144 | 30.58 | 30.92 |
| 65,536 | 8 | overlap | 262,144 | 30.67 | 30.84 |
| 65,536 | 8 | disjoint | 2,097,152 | 292.57 | 292.93 |

The added sets scale with unique shard references, not payload paths. They add small per-pass bookkeeping but do not shrink the retained-path set. At the larger size, shared roots still retain 22.75 MiB of path characters; disjoint roots retain 182 MiB and increase RSS by about 293 MiB. The measurements do not demonstrate a meaningful memory improvement.

RSS is a before/after process measurement, not peak heap or an exact allocation count. It includes allocator reuse and shard caches; the fixture keeps its independent expected-path set alive. The in-memory backend and memory pin ledger exclude durable-ledger and payload-I/O costs. Roots contain sharded bases without run overlays. Root-map reads, shared runs and checkpoint/inline-base decoding are not deduplicated by this change. No new threshold or performance cliff was found in this matrix.

## Reproduce and provenance

```console
cargo test --locked --offline --release --features cli --lib --no-run -j 2
# Save the baseline executable before applying the runtime change.
# Rebuild and save the candidate executable separately.
benchmark run catalog-marking --profile standard --repetitions 6 --baseline-binary /absolute/path/to/baseline --probe-binary /absolute/path/to/candidate --no-build --output paired.json
benchmark all --suites catalog-marking --repetitions 1 --bin-dir /absolute/path/to/bin --output /tmp/catalog-marking-smoke
```

Baseline runtime: `62ea96d5f167a03233a50c0a97773d0f25ea4ce1` plus the permanent benchmark probe from the baseline investigation. Its executable hash matches the saved baseline report. Candidate: the shared-shard marking implementation in this change, using the same benchmark fixture. Both use the same compiler version and release/cli build configuration. Builds used the cached devenv shell and `/tmp/casita-online-holds-build` as Cargo build directory. Our builds and tests completed before measurement; unrelated host activity remained.

Executable SHA-256:

- baseline: `8c3747a297968a5852d0cc2f444c7585ceddd316bf8d0212ebbae6ebf44a63cf`
- candidate: `324aeb759c94933708a4744f82bae01a40d1038370b4fc79d493732b5398df82`

The raw report includes all process output, binary hashes, invocation-time source hashes, compiler/host metadata, timing, RSS and exact I/O/path counts. Source hashes describe the candidate checkout; executable hashes distinguish the two runtimes.

Validation: 96/96 release samples; `benchmark all` candidate smoke integration; 71 pack tests; 25 transfer tests; 52 collection tests (one ignored probe); 240 Python tests; all-feature/all-target Clippy with warnings denied; formatting and diff checks. The first collection invocation lacked the devenv `rustfs` executable; all four affected tests passed on rerun in the development environment.

Next preference: measure end-to-end GC with several overlapping live holds to determine how much of the marking gain reaches real repository workloads. A quieter-host control rerun is the alternative if certifying the 20% ceiling is required before adoption. Compact retained-path storage remains a separate memory investigation.

[Raw paired measurements](2026-09-13-catalog-marking-dedup.json)
