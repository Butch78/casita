# Batched cleanup without physical narrowing

Physical narrowing has been removed. Historical catalog pins continue to protect all their packs until release. The production change only batches published retirements, orphan cleanup and replacement-pack recovery through the existing cancellation-safe deletion-claim path, with a limit of 1,000 paths. Public APIs and logical online-hold scopes are unchanged.

For the large selected case with earlier GC, median cleanup after release fell from 377.9 to 188.0 ms locally and from 369.7 to 188.8 ms over SSH-stdio (about 50% lower). Journal syncs fell from 54 to 14. All 16 post-release median comparisons improved, but wide ranges on this shared host limit the precision of timing claims. During selected reads both versions use eight journal syncs; batching does not add cleanup work there.

## GC after releasing the hold

Milliseconds: median (minimum–maximum), three samples per cell. Every case reclaims all source packs. “Earlier GC” means a collection was also run while the source read was open.

| Transport | Payload | Scope | Earlier GC | Before ms | Batched ms | Before syncs | Batched syncs |
| --- | --- | --- | --- | ---: | ---: | ---: | ---: |
| local | 4096 B | snapshot | False | 309.2 (86.8–312.3) | 97.8 (47.7–163.1) | 46 | 12 |
| local | 4096 B | snapshot | True | 305.3 (83.9–326.2) | 60.3 (46.1–168.0) | 46 | 12 |
| local | 4096 B | selected | False | 304.8 (81.8–313.0) | 62.4 (44.4–168.4) | 46 | 12 |
| local | 4096 B | selected | True | 302.7 (82.4–305.6) | 53.7 (43.4–152.2) | 46 | 10 |
| local | 4194304 B | snapshot | False | 419.1 (130.6–516.3) | 188.7 (59.6–199.3) | 54 | 14 |
| local | 4194304 B | snapshot | True | 380.3 (126.0–415.7) | 203.9 (65.6–239.5) | 54 | 14 |
| local | 4194304 B | selected | False | 450.8 (127.8–549.5) | 220.9 (60.1–235.5) | 54 | 14 |
| local | 4194304 B | selected | True | 377.9 (125.1–378.0) | 188.0 (60.9–213.7) | 54 | 14 |
| ssh-stdio | 4096 B | snapshot | False | 296.1 (83.7–307.3) | 49.7 (46.3–167.1) | 46 | 12 |
| ssh-stdio | 4096 B | snapshot | True | 320.7 (84.2–353.4) | 45.1 (43.9–164.6) | 46 | 12 |
| ssh-stdio | 4096 B | selected | False | 313.5 (79.7–340.1) | 47.6 (44.8–162.4) | 46 | 12 |
| ssh-stdio | 4096 B | selected | True | 308.1 (78.3–319.0) | 38.9 (37.6–161.7) | 46 | 10 |
| ssh-stdio | 4194304 B | snapshot | False | 386.2 (134.7–408.9) | 228.6 (68.7–238.4) | 54 | 14 |
| ssh-stdio | 4194304 B | snapshot | True | 366.2 (134.5–405.7) | 191.6 (62.7–246.9) | 54 | 14 |
| ssh-stdio | 4194304 B | selected | False | 374.1 (131.6–390.3) | 220.6 (64.4–227.4) | 54 | 14 |
| ssh-stdio | 4194304 B | selected | True | 369.7 (128.2–375.2) | 188.8 (60.4–195.2) | 54 | 14 |

## GC while a selected read remains open

| Transport | Payload | Before ms | Batched ms | Before syncs | Batched syncs |
| --- | --- | ---: | ---: | ---: | ---: |
| local | 4096 B | 86.3 (29.1–87.1) | 35.5 (31.8–86.6) | 8 | 8 |
| local | 4194304 B | 74.1 (25.3–150.6) | 73.5 (28.5–80.8) | 8 | 8 |
| ssh-stdio | 4096 B | 92.5 (27.7–95.9) | 28.6 (28.1–90.1) | 8 | 8 |
| ssh-stdio | 4194304 B | 73.9 (25.2–74.3) | 72.1 (23.6–78.4) | 8 | 8 |

## Method and validation

All 96 cases passed (48 per binary). The permanent `transfer-holds` suite records GC timing, phases and journal counts both during a read and after release, plus acquisition/copy latency. Correctness gates check exact transferred bytes, retained revision, logical reclamation, historical-pack retention during the large selected read and complete physical reclamation after release. Its existing manifest registration includes it in `benchmark all`. The small and large fixtures cover mixed and separate pack layouts.

Before uses runtime source from the committed revision recorded in the raw data. Both binaries use exactly the same benchmark source and Cargo.lock. Execution order alternates between binaries across three matched rounds; each runner invocation uses one repetition and the same snapshot-first scope order. All samples are retained. Shared-host timing variation and three repetitions limit generalization; the sync counts directly measure work removed. SSH runs the real stdio protocol over an in-process duplex stream, excluding network RTT, encryption and process startup.

A preliminary baseline run failed the historical-pack gate because its build reused a stale library. It is excluded; the baseline library was explicitly invalidated and rebuilt before the six successful runs.

Passed: 68 pack tests, 25 transfer tests, 52 collection tests (one benchmark ignored), all-feature/all-target Clippy with warnings denied, formatting and whitespace checks. The retained regressions exercise 999/1,001 retired and orphan paths, partial failures, claim ownership, paused payload/proof reads, independent collector handles, local/SSH transfers and mixed/separate packs.

```sh
cargo bench --locked --offline --features ssh,experimental --bench transfer_holds --no-run -j 2
# Save each reported executable as transfer_holds in separate before/after directories.
# For rounds 0, 1, 2, run before/after, after/before, before/after respectively:
python3 -m benchmarks.cli all --suites transfer-holds --repetitions 1 \
  --bin-dir /tmp/casita-batching-only-before-bin \
  --output /tmp/casita-batching-only-validated/before-0
# Substitute the binary label and round for the remaining five invocations.
```

[Raw samples, source hashes, environments, binary hashes and completion ledgers](2026-09-13-batching-only.json). The removed physical-narrowing experiment remains documented in [its initial report](2026-09-13-scoped-catalog-gc.md) and [cleanup investigation](2026-09-13-batched-cleanup.md).
