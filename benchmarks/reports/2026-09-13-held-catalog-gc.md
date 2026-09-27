# Whole local GC with overlapping historical holds

For reproduction after packed-reader integration, first reconstruct the
[historical source](2026-09-14-gc-source-reproduction.md), then use this report's commands.

**No convincing whole-GC speedup at these fixture sizes.** The targeted final payload-cleanup phase consistently improves with eight holds, but it is a small fraction of total collection time. At 1,024 blobs and eight holds its median falls from **5.62 to 2.62 ms**, about **3 ms saved**. Whole-GC medians are **212.80 ms before and 222.97 ms after** (+4.8%), while the median paired change is −6.3%. Those different summaries and broad ranges make an overall speedup claim unwarranted.

Keep the narrow shard-deduplication change for its demonstrated large-catalog marking benefit, without presenting the earlier 7.5× marking result as a whole-GC speedup. The next preferred investigation is `finish_deletions` inside `sweep_payloads`, which takes about 149 ms in the large eight-hold candidate case. This turn adds benchmark coverage and evidence, not another runtime optimization.

## Workload and correctness

- Real local repositories on Btrfs SSD storage, with the ordinary durable pin ledger, metadata store, compaction and physical cleanup. Each fixture starts with 128 or 1,024 deterministic, incompressible 4 KiB blobs, one named root, and otherwise unrooted objects.
- Setup uses the existing test-only rebase threshold to obtain a sharded catalog at bounded sizes, then flushes background work and reopens the repository with normal settings. This does not measure the production catalog-compaction threshold or claim a naturally sharded fresh 128-object repository.
- One or eight selected transfer sessions hold the same object. Each later session follows publication of one extra unrooted blob, producing a distinct historical root over the same sharded base. The named root is removed before measurement.
- Every sample asserts exactly one distinct held catalog per session and one common sharded base. It pauses real streams after their first byte, then calls ordinary `Repository::try_collect` with the sessions and readers alive.
- GC must remove exactly `count + holds - 2` unrelated logical objects and preserve every original pack file at its original length. GC may additionally create a compacted current pack. All paused streams and fresh streams opened through every held session must return the complete expected payload after GC.
- After all sessions and readers are dropped and lease releases are flushed, a separately timed GC must remove the final logical object and reclaim every physical pack.

All **48 samples** passed every gate: six repetitions × two sizes × two hold counts × two runtimes. Each sample includes both held and released GC. No active hold blocks the collector in these cases.

## GC while holds remain live

Six paired repetitions, alternating baseline/candidate order each repetition. Each sample has fresh fixture storage. Setup, readback and pack-file correctness checks are outside the timers.

| Blobs | Holds | Before ms, median (range) | After ms, median (range) | Change in medians | Median paired change |
|---:|---:|---:|---:|---:|---:|
| 128 | 1 | 81.33 (24.81–108.51) | 79.38 (24.50–93.52) | -2.4% | -1.3% |
| 128 | 8 | 182.31 (60.49–241.59) | 172.65 (59.82–247.49) | -5.3% | -1.5% |
| 1,024 | 1 | 103.88 (47.29–161.77) | 100.85 (36.63–144.03) | -2.9% | -22.2% |
| 1,024 | 8 | 212.80 (79.04–303.56) | 222.97 (69.55–287.09) | +4.8% | -6.3% |

Negative changes mean faster. “Change in medians” divides the timing medians; “median paired change” first divides candidate by baseline within each repetition. The one-hold 1,024-blob control also shows a large apparent paired win (−22.2%) despite almost unchanged final-cleanup timing. That is another reason not to attribute whole-run differences to shard deduplication. Individual excursions remain too large to certify a strict 20% overhead ceiling.

## Targeted phase and dominant work

These are instrumented GC phases, not a separate pure-marking timer. `finish_payload_collection` includes marking and subsequent physical cleanup. `finish_deletions` is nested inside `sweep_payloads`; do not add these two columns together or sum all raw phase events.

| Blobs | Holds | Final payload cleanup before → after, ms | Payload sweep before → after, ms | Finish deletions before → after, ms |
|---:|---:|---:|---:|---:|
| 128 | 1 | 1.02 → 1.00 | 45.16 → 42.29 | 40.29 → 37.82 |
| 128 | 8 | 2.19 → 1.60 | 142.41 → 134.92 | 137.55 → 130.17 |
| 1,024 | 1 | 1.74 → 1.69 | 58.78 → 56.67 | 52.77 → 50.80 |
| 1,024 | 8 | 5.62 → 2.62 | 155.94 → 161.31 | 144.90 → 148.99 |

Final payload cleanup improved in eleven of the twelve eight-hold pairs, including all six larger cases. The remaining small pair changed from 9.964 to 9.996 ms (effectively unchanged). At 1,024 blobs the saved 3 ms is roughly 1.4% of the baseline whole-GC median. This fraction describes where the measured saving sits; it is not a prediction of universal end-to-end improvement. The much larger sweep/deletion variability dominates the whole-run totals.

## GC after release

| Blobs | Prior holds | Before median ms | After median ms | Median paired change |
|---:|---:|---:|---:|---:|
| 128 | 1 | 81.98 | 82.60 | +2.0% |
| 128 | 8 | 104.34 | 107.27 | +2.8% |
| 1,024 | 1 | 91.18 | 85.55 | -5.9% |
| 1,024 | 8 | 116.62 | 127.76 | +3.8% |

Every released sample ends with zero pack bytes. These timings describe deferred reclamation after readers finish; the historical-root deduplication is not expected to improve this phase substantially.

## Scope and limitations

- This is a high-garbage local workload with one selected live blob. It does not measure multiple independently selected graphs, concurrent writer throughput, S3/SSH GC or large naturally accumulated run chains.
- Normal packing puts many chunks in each initial pack. This differs materially from the earlier isolated fixture with one synthetic pack per entry and up to 65,536 entries. The two benchmark speedup ratios are not directly interchangeable.
- Compare runtimes within each fixed size/hold count. Comparing one hold directly with eight also changes the number of later publications and the amount of read-cache warming.
- Our builds and test runs finished before measurement, but this is a shared host and local storage latency varied. Alternating binary order reduces ordering bias; it cannot remove contention, filesystem variability or thermal effects. No new performance threshold was located.
- Filesystem used by fixture temporary directories: `btrfs /dev/mapper/encryptedroot rw,noatime,compress=zstd:3,ssd,discard=async,space_cache=v2,subvolid=5,subvol=/`.

## Reproduce

`held-catalog-gc` is registered in `benchmarks/manifest.json`, included in `benchmark all`, and supports prebuilt paired library test binaries.

```console
# Build the candidate from this change and save the printed library test executable.
cargo test --locked --offline --release --features cli --lib --no-run -j 2
# In a separate checkout of this same change, restore only the old marking code:
git apply benchmarks/reports/2026-09-13-held-catalog-gc-baseline.patch
# Build and save that baseline using a separate build directory.
cargo --config 'build.build-dir="/tmp/held-gc-baseline-build"' test --locked --offline --release --features cli --lib --no-run -j 2
# Run from the candidate checkout with the two saved executables:
benchmark run held-catalog-gc --profile standard --repetitions 6 --baseline-binary BEFORE --probe-binary AFTER --no-build --output paired.json
benchmark all --suites held-catalog-gc --repetitions 1 --bin-dir BIN_DIRECTORY --output /tmp/held-catalog-gc-smoke
```

The `benchmark all` binary directory must contain `casita-lib-test`. The baseline patch restores the pre-deduplication methods from `62ea96d5f167a03233a50c0a97773d0f25ea4ce1` and removes the regression test that requires the new helper; the GC fixture is byte-identical in both builds. Both builds used the same lockfile and cached devenv environment. The actual run reused `/tmp/casita-online-holds-build` sequentially and explicitly forced a baseline source rebuild; the build log identified the baseline checkout. The baseline lacks the deduplication-specific test, the candidate includes it, and both contain the end-to-end probe. Separate build directories are recommended for reproduction to avoid cross-checkout Cargo cache reuse.

Executable SHA-256:

- baseline: `4b0b4e4b4c551362b7053eeea25ceb65cb16324c9e6b357cb6edac82dc2149b9`
- candidate: `ca1e1277232c0dffa104e9d12309dfea0795c09cd6f34350a3cbc1daf1ca3b88`

Fixture source SHA-256 (both builds): `58ccf5593a91ff9cf714856c4f1e26003fe04e01f15336d28713d7bbd26bcb2e`.

Validation: 48/48 paired release samples; both small cases through `benchmark all`; both larger candidate cases in the test build; 243 Python tests; all-feature/all-target Clippy with warnings denied; formatting and diff checks. Production runtime behavior was unchanged during this follow-up investigation.

[Raw samples and process logs](2026-09-13-held-catalog-gc.json) · [Exact baseline patch](2026-09-13-held-catalog-gc-baseline.patch) · [Earlier marking-only comparison](2026-09-13-catalog-marking-dedup.md)

### Reproducing after the later per-marker change

The later [individual-marker comparison](2026-09-13-gc-local-marker-put.md) changed
local marker publication. To reproduce this report's earlier runtime from the
current source, first apply `2026-09-13-gc-local-marker-put-baseline.patch` in an
isolated copy, then follow this report's commands and any experiment-specific patch.
