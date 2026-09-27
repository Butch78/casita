# Scoped catalog protection during online GC

> Historical experiment: physical narrowing was subsequently removed. Its measurements and correctness gates below describe the experimental binaries, not the current implementation. See the [batching-only comparison](2026-09-13-batching-only.md) for the retained change.


The change allows GC to reclaim garbage-only historical packs during a selected transfer. In all 12 large selected/GC cases (local and SSH, six repetitions each), pack storage fell from 8,390,752 to 4,195,154 bytes while the stream remained open: 4,195,598 bytes reclaimed, approximately half. The saved baseline reclaimed zero pack bytes during the same cases. Both versions reclaimed all source packs after release.

This is a storage-retention improvement, not a demonstrated speed improvement. Selected GC passes were roughly 3–4 times longer in this fixture because the new path also performs physical cleanup. Copy timings varied substantially, including in snapshot controls; these runs do not establish a speedup or verify a 20% copy-overhead ceiling.

## Change and safety boundaries

GC keeps its existing spilling live-payload and live-chunk sets through physical cleanup. For closure-scoped catalog pins, historical payload marking filters catalog entries through those sets. It still protects the historical locations of live chunks, preserving old readers after repacking. Historical catalog metadata remains protected. No transfer API, SSH protocol, or eager graph discovery was added.

Filtering requires the same collector, no active logical-prune fence, and the same payload protections as the validated mark. Otherwise marking retains complete catalogs. Subsequent pin changes invalidate physical deletion through the existing claim checks. Snapshot pins, staging catalogs and explicit catalog resources remain conservative. Emergency cleanup before publication has no narrowing mark. Repairing stores forward the opaque mark to their collecting backend. The experimental `BlobGc::finish_collection_pinned` hook gains an optional `CollectionMark`; application APIs are unchanged.

A mixed historical pack containing a selected live chunk must still remain. The 4 KiB fixture continued to retain its mixed pack and add 4,186 bytes during compaction, just as before. The optimization also conservatively uses the global live set rather than performing another graph traversal per pin. It can therefore retain extra historical representations of other still-live data.

## Measurement method

The permanent `transfer-holds` corpus ran six repetitions per binary (96 cases each), with snapshot/selected order alternating within each cohort. The new cohort ran first, then the saved baseline binary was rerun on the current machine; binaries were not interleaved. Its SHA-256 was verified against the original committed report. All samples are retained. Repository construction, staging, correctness checks and controlled GC overlap are outside the copy timer.

Fixtures and timed operations match the original corpus. The only benchmark-code change adds an untimed assertion requiring at least 3 MiB of pack reclamation in the 4 MiB selected/GC case. The 4 KiB and 4 MiB cases straddle the local 4 MiB pack target. Separate and mixed layouts are additionally constructed explicitly in regression tests. Measurements sum pack-file lengths, excluding metadata, outboards and allocated filesystem blocks.

The SSH case runs the actual stdio protocol over an in-process duplex stream; network RTT, encryption and process startup are excluded. Both cohorts used the shared Ryzen 7 7840S/Btrfs machine and Rust 1.96.0. Wide ranges and sequential cohorts limit causal timing conclusions. Large or spilled-catalog performance was not measured by this small fixture; spill correctness is covered by the regression test.

## GC duration

Milliseconds: median (minimum–maximum), six samples per cell. Selected cleanup performs more physical work after the change.

| Transport | Payload | Scope | Before | After |
| --- | ---: | --- | ---: | ---: |
| local | 4 KiB | snapshot | 17.35 (9.13–32.57) | 12.54 (5.66–15.50) |
| local | 4 KiB | selected | 100.26 (45.30–197.10) | 358.14 (111.18–408.88) |
| local | 4 MiB | snapshot | 17.19 (13.69–28.87) | 17.56 (6.70–27.09) |
| local | 4 MiB | selected | 80.09 (63.96–133.21) | 347.48 (115.57–663.26) |
| ssh-stdio | 4 KiB | snapshot | 14.36 (11.92–30.55) | 13.11 (5.49–36.00) |
| ssh-stdio | 4 KiB | selected | 107.63 (86.81–156.23) | 321.17 (113.08–379.83) |
| ssh-stdio | 4 MiB | snapshot | 17.77 (13.27–45.47) | 21.83 (6.67–57.87) |
| ssh-stdio | 4 MiB | selected | 93.39 (75.99–126.00) | 384.37 (284.62–565.24) |

## Copy latency without GC

Milliseconds: median (minimum–maximum); excludes acquisition and GC duration.

| Transport | Payload | Scope | Before | After |
| --- | ---: | --- | ---: | ---: |
| local | 4 KiB | snapshot | 316.88 (145.77–643.53) | 240.48 (189.94–551.91) |
| local | 4 KiB | selected | 214.51 (153.04–734.48) | 285.31 (136.69–535.41) |
| local | 4 MiB | snapshot | 445.68 (389.08–702.25) | 443.61 (165.88–568.27) |
| local | 4 MiB | selected | 520.51 (377.62–1786.19) | 410.95 (176.94–3027.85) |
| ssh-stdio | 4 KiB | snapshot | 264.04 (156.47–690.48) | 419.04 (79.88–1463.87) |
| ssh-stdio | 4 KiB | selected | 227.15 (117.09–387.97) | 580.44 (78.19–1379.80) |
| ssh-stdio | 4 MiB | snapshot | 515.13 (377.50–676.07) | 651.16 (174.77–1549.64) |
| ssh-stdio | 4 MiB | selected | 459.65 (384.00–1111.20) | 699.91 (227.30–1258.05) |

## Copy latency after controlled GC overlap

Milliseconds: median (minimum–maximum); excludes acquisition and GC duration.

| Transport | Payload | Scope | Before | After |
| --- | ---: | --- | ---: | ---: |
| local | 4 KiB | snapshot | 218.38 (143.79–916.52) | 201.60 (76.10–457.65) |
| local | 4 KiB | selected | 269.17 (127.32–360.01) | 370.96 (136.55–516.37) |
| local | 4 MiB | snapshot | 498.37 (368.98–896.35) | 502.39 (160.12–780.72) |
| local | 4 MiB | selected | 482.73 (370.67–609.78) | 491.11 (198.08–1244.19) |
| ssh-stdio | 4 KiB | snapshot | 214.02 (156.65–293.46) | 342.74 (89.59–921.87) |
| ssh-stdio | 4 KiB | selected | 242.47 (167.72–708.28) | 310.45 (99.46–2065.94) |
| ssh-stdio | 4 MiB | snapshot | 551.33 (398.77–742.58) | 513.80 (223.26–1155.32) |
| ssh-stdio | 4 MiB | selected | 736.83 (385.93–1176.18) | 767.89 (208.95–1054.72) |

## Validation and reproduction

All-feature/all-target Clippy with `-D warnings`, formatting and diff-whitespace
checks passed.

Both 96-case benchmark cohorts completed successfully. The new benchmark additionally gates physical reclamation during the live stream. Targeted Rust suites passed: 68 pack tests, 25 transfer tests, 16 SSH tests, 10 object-read tests and 52 collection-filtered tests (some names overlap between suites). The new regression cases cover independent collector handles, paused payload and Bao reads, copying after GC, final release, separate/mixed packs, inline/sharded catalogs, spilled live sets, changed pins, and explicit catalog protection.

```sh
cargo bench --offline --features ssh,experimental --bench transfer_holds --no-run -j 2
# Copy the reported executable to a dedicated directory as transfer_holds.
benchmark all --suites transfer-holds --repetitions 6 \
  --bin-dir /tmp/casita-scoped-catalog-bin \
  --output /tmp/casita-scoped-catalog-gc-2026-09-13
# Repeat with the saved baseline executable:
benchmark all --suites transfer-holds --repetitions 6 \
  --bin-dir /tmp/casita-transfer-holds-bin \
  --output /tmp/casita-scoped-catalog-baseline-2026-09-13
```

[Raw samples, environment records, completion ledgers and binary hashes](2026-09-13-scoped-catalog-gc.json). Acquisition latency and throughput are retained in the raw samples alongside the tables above. The benchmark remains registered in `benchmarks/manifest.json` and included in `benchmark all`.

## Cleanup follow-up

The [batched-cleanup investigation](2026-09-13-batched-cleanup.md) identifies repeated per-file deletion claims as a major cost and compares bounded cleanup batches against both this implementation and the original retained-pack behavior.
