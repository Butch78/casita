# Batched physical cleanup during online GC

> Historical experiment: physical narrowing was subsequently removed. Its measurements and correctness gates below describe the experimental binaries, not the current implementation. See the [batching-only comparison](2026-09-13-batching-only.md) for the retained change.


Payload cleanup was claiming and synchronizing each retired file individually. Batching now covers published retirements, unreferenced uploads and replacement-pack recovery, using the existing cancellation-safe deletion path. Each batch validates the pin inventory, claims its resources and durably deletes files before releasing its claim. Batches contain at most 1,000 paths. Failed retirement batches remain queued for retry, with claims retained on deletion failure.

Batching removes redundant synchronization, but does not resolve the original tradeoff: large selected-GC medians remain 3.9× the original for local and 2.6× for SSH-stdio. Relative to serial physical cleanup, three of four selected medians improve by 42–57%; the large local median worsens by 21%. With three samples and wide ranges, these are observations rather than stable speedup estimates. Keep the physical-narrowing change unmerged pending a better cleanup-cost result.

## Results

Three samples per cell; GC milliseconds are median (minimum–maximum). The original binary retains whole historical packs during selected reads; serial and batched perform narrowed physical cleanup.

| Transport | Payload | Scope | Original | Serial cleanup | Batched cleanup |
| --- | --- | --- | ---: | ---: | ---: |
| local | 4096 B | snapshot | 18.1 (11.1–25.5) | 16.5 (15.0–17.4) | 15.5 (12.2–17.5) |
| local | 4096 B | selected | 179.1 (159.5–193.2) | 597.2 (576.8–621.7) | 333.9 (190.0–361.2) |
| local | 4194304 B | snapshot | 35.0 (12.5–37.9) | 18.8 (15.9–22.4) | 19.6 (15.4–25.9) |
| local | 4194304 B | selected | 138.0 (128.9–150.0) | 442.9 (282.6–640.3) | 534.5 (189.9–693.4) |
| ssh-stdio | 4096 B | snapshot | 23.5 (15.4–26.2) | 15.5 (11.6–16.4) | 13.2 (13.0–142.7) |
| ssh-stdio | 4096 B | selected | 216.8 (205.5–217.1) | 571.1 (272.0–620.5) | 333.4 (201.6–1002.2) |
| ssh-stdio | 4194304 B | snapshot | 32.6 (16.1–33.0) | 20.5 (15.0–20.9) | 14.9 (13.3–15.7) |
| ssh-stdio | 4194304 B | selected | 128.5 (121.9–184.5) | 786.3 (594.4–834.9) | 337.1 (187.1–390.9) |

Selected GC cleanup phase and journal sync counts:

| Transport | Payload | Serial cleanup ms | Batched cleanup ms | Serial syncs | Batched syncs |
| --- | --- | ---: | ---: | ---: | ---: |
| local | 4096 B | 425.5 (424.3–459.1) | 170.6 (98.7–190.1) | 44 | 12 |
| local | 4194304 B | 352.8 (224.3–497.5) | 345.0 (107.7–555.3) | 46 | 12 |
| ssh-stdio | 4096 B | 442.5 (198.9–466.7) | 174.4 (94.9–573.3) | 44 | 12 |
| ssh-stdio | 4194304 B | 597.4 (475.8–706.8) | 187.6 (95.1–265.8) | 46 | 12 |

All 48 cases passed in each cohort. Batched large selected cases reclaimed 4,195,598 pack bytes while a read remained open; the original reclaimed none. Small selected cases retain a mixed historical pack and add 4,186 bytes during compaction. Snapshot controls retain all data. Every case verifies copied payloads and complete pack removal after releasing the hold.

Sequential cohorts on a shared Btrfs host have substantial timing variation. These measurements establish the reduction in synchronization work and preserve the physical-reclamation benefit; they do not certify a 20% timing-overhead ceiling relative to the original. Copy timings, acquisition timings, environment records, binary hashes and all samples are retained in the raw report. SSH uses the real stdio protocol over an in-process duplex stream, excluding network RTT, encryption and process startup.

The first partial fix batched only published retirements. Its benchmark failed the new 16-sync gate at 38 syncs, exposing the orphan loop; those failed runs are excluded from the successful comparison. The gate remains unchanged in the final corpus.

## Validation and reproduction

The permanent `transfer-holds` suite remains registered in `benchmarks/manifest.json` and included in `benchmark all`. It records GC phases and ledger counts, caps selected-case journal syncs at 16 and requires at least 3 MiB of reclamation for the large selected case. The 4 KiB and 4 MiB fixtures cover both sides of the pack-layout threshold. Unit tests cover retired and orphan batches of 999 and 1,001 paths, partial deletion failure, retained claims and lost ownership.

Passed: 69 pack tests, the live-transfer regression (eight local/SSH and separate/mixed-layout cases), 52 collection tests (one benchmark ignored), all-feature/all-target Clippy with warnings denied, formatting and whitespace checks. The first direct collection-test invocation lacked the devenv PATH and four RustFS tests could not start; rerunning in the cached shell passed.

```sh
cargo bench --offline --features ssh,experimental --bench transfer_holds --no-run -j 2
# Preserve each reported executable as transfer_holds in a dedicated directory.
python3 -m benchmarks.cli all --suites transfer-holds --repetitions 3 \
  --bin-dir /tmp/casita-cleanup-batched-bin \
  --output /tmp/casita-cleanup-batched-profile
# Repeat with preserved serial and original binaries for the other cohorts.
```

[Raw results and provenance](2026-09-13-batched-cleanup.json). This follows the [initial scoped-catalog result](2026-09-13-scoped-catalog-gc.md).
