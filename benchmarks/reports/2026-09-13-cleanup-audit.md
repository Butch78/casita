# Cleanup batching audit

The audit covers online admission, cancellation, partial failure, exact-token recovery, durability, memory and the 1,000-path boundary. Physical narrowing remains removed. Existing active catalog holds conservatively retain their historical packs.

## Findings

| Area | Finding |
| --- | --- |
| Partial local deletion | Found and fixed a durability gap. If unlinking an earlier file succeeded and a later unlink or directory sync failed, retry skipped already missing files and could release its claim without syncing their directories. Both single and batch retries now sync those paths, walking to a surviving ancestor when directories are absent. |
| Failure recovery | An unfinished claim keeps the collector unsettled. The standard local profile can automatically recover after acquiring its filesystem collector fence, which proves old local I/O has stopped. Generic/remote backends require explicit exact-token recovery when they cannot establish this automatically. Batching increases the resources covered by a failed claim. |
| Reader/writer admission | Claims conflict with their exact resources. A paused 1,000-path deletion rejects overlapping admission while accepting a disjoint staging pin and a metadata pin. This is logical admission isolation, not a promise of zero disk contention. |
| Cancellation | Cancelling the low-level cleanup caller does not cancel already claimed I/O. The tracked task settles the current batch; the retirement guard preserves pending work. Public collection additionally runs in a tracked task, so cancellation is not an immediate stop request. Backend latency has no hard bound. |
| Changes to pins | A changed payload mark defers remaining ordinary physical cleanup. Emergency cleanup still fails on a stale mark. Repeated changes can defer physical reclamation across passes; cleanup does not wait for the holds to end. |
| Space recovery | An active historical catalog can retain unrelated packs. Releasing a pin during an active collector retains its history until that collector exits. If collection fails, recovery is needed before that retired history is cleared. Mixed packs and held catalogs therefore may delay space recovery. |
| Memory | Each claim is limited to 1,000 paths, not a byte budget. Existing pin inventories and the retirement queue can be large; cleanup still snapshots the entire retirement path set before batching. Batching does not introduce a new asymptotic allocation, but it is not a global GC memory bound. |
| Storage failures | Deletion errors preserve claims and may leak space until retry/recovery. The durability fix concerns potential reappearance of garbage after power loss, not deletion of live data. Tests observe actual directory-sync calls and process death; they do not simulate a power cut or storage firmware. |

## Regression evidence

The new local regression fails on the old implementation after a partial deletion. It checks syncing both the already missing and newly removed paths, deduplication of the shared root, a single-file retry and a path whose parent directories never existed.

The cancellation regression now uses 1,001 candidates, checks a 1,000-resource in-flight claim, rejects overlapping admission, accepts unrelated staging/metadata pins, cancels the caller, drains tracked I/O, then retries the remaining candidates. Existing tests cover partial batch failure, ownership loss, changed pins, exact-token recovery, mixed/separate packs and paused payload/proof reads from an independent repository handle.

## Performance evidence

The permanent `cleanup-batches` suite is registered in `benchmarks/manifest.json`, has a revision-build contract and participates in `benchmark all`. It uses real local filesystem cleanup and a file pin ledger at 999 and 1,001 garbage paths. A disjoint held file must survive; all candidates and deletion claims must disappear. The ledger must advance by exactly one claim/release pair per batch. Setup and correctness checks are outside the timer.

```sh
cargo test --locked --offline --all-features --lib blob:: -- --test-threads=2
cargo bench --locked --offline --features ssh,experimental --bench transfer_holds --no-run -j 2
# Preserve the library test executable as casita-lib-test and the release benchmark
# as transfer_holds in a dedicated directory, then run:
python3 -m benchmarks.cli all --suites cleanup-batches --repetitions 3 \
  --bin-dir /tmp/casita-batch-audit-bin --output /tmp/casita-batch-audit-threshold
```

The boundary probe uses an unoptimized library test executable in this investigation: timings include debug bookkeeping and are not throughput estimates. File deletion and pin-ledger synchronization are real. The module's default build and revision contract use a release test executable for future runs. Transfer benchmarks use optimized executables.

## Measured results

Milliseconds: median (minimum–maximum). No threshold cliff was demonstrated in these three samples; the claim count changes exactly as intended.

| Garbage paths | Cleanup ms | Claim/release pairs |
| ---: | ---: | ---: |
| 999 | 1019.8 (1007.7–1029.1) | 1 |
| 1001 | 987.4 (337.9–1027.5) | 2 |

Selected cases with an earlier GC during the hold; post-release cleanup before versus after the durability fix:

| Transport | Payload | Before ms | Fixed ms | Before/fixed journal syncs |
| --- | --- | ---: | ---: | --- |
| local | 4096 B | 152.1 (45.3–155.9) | 74.6 (39.1–283.9) | 10 → 10 |
| local | 4194304 B | 192.0 (56.6–328.6) | 82.4 (61.1–562.3) | 14 → 14 |
| ssh-stdio | 4096 B | 144.6 (43.0–150.2) | 51.6 (42.3–257.0) | 10 → 10 |
| ssh-stdio | 4194304 B | 210.3 (59.7–330.5) | 218.0 (61.4–448.9) | 14 → 14 |

All six boundary samples and all 96 transfer cases passed. Journal counts in the selected post-release cases remain unchanged by the durability fix. The fix may perform extra directory syncs for already missing paths; journal counts do not measure those filesystem syncs. Wide shared-host ranges and three repetitions do not establish a strict 20% latency ceiling. Acquisition, active-GC, copy and all other post-release timings are retained in the raw report.

Validation passed: 201 blob tests (12 ignored probes), 25 transfer tests, 52 collection tests (one ignored probe), 231 Python benchmark tests, all-feature/all-target Clippy with warnings denied, formatting and whitespace checks. The old code failed the new directory-sync regression before the fix.

[Raw samples, environments, completion ledgers and hashes](2026-09-13-cleanup-audit.json).

## Retirement queue follow-up

The [bounded-iteration change](2026-09-13-retirement-queue.md) subsequently removed the full temporary path-vector copy discussed above. The queue and its hash-table allocation still scale with the number of retirements; only temporary path copies are bounded by batch size.
