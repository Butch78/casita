# Transfer retention policies: local disk and SSH stdio

Selected holds improve logical reclamation during a transfer, but this fixture shows no physical storage savings while the session remains open. These runs do not establish a transfer speedup or a 20% overhead bound. Keep the logical-retention benefit; if physical reclamation during long transfers matters, the remaining catalog retention deserves the next focused investigation.

## Method

Six repetitions of all 16 cases, alternating snapshot/selected order, on commit `f9d053a5f440221e2a6718a2c6fa7856a3249caf` plus the benchmark additions. Both policies run in the same release binary. Each case creates fresh local Turso/chunked repositories, one selected named blob (4 KiB or 4 MiB), and 16 unrelated unrooted 256 KiB blobs. Payloads are deterministic and incompressible. Construction and staging are untimed. The source root is removed after acquisition, so only the live session protects its old value.

SSH stdio uses the actual protocol over a bounded 64 KiB in-process duplex connection. Process startup, encryption and network latency are excluded. Timed copies include destination publication and use fresh destinations. This is an end-to-end small fixture, not a saturation throughput test or a historical binary comparison. Cache eviction is not performed.

GC cases pause a payload stream after one byte, collect, verify the remainder, then time a copy. Their source cache is therefore warmer than the no-GC cases; compare policies within each setting. The GC timer is separate from copy time. Physical bytes are file lengths under `blobs/packs`, excluding metadata, catalogs and allocated-block accounting.

The shared machine was an AMD Ryzen 7 7840S (16 logical CPUs), Linux 7.2.2, Btrfs, performance governor, Rust 1.96.0. Large timing ranges prevent firm performance conclusions. All samples are retained, with no outlier filtering.

## Copy latency without GC

Milliseconds: median (minimum–maximum), six samples per cell. Session acquisition is excluded.

| Transport | Payload | Snapshot | Selected |
| --- | ---: | ---: | ---: |
| local | 4 KiB | 239.62 (178.05–1014.47) | 225.29 (155.12–364.82) |
| local | 4 MiB | 838.30 (628.48–2261.16) | 705.73 (459.81–1407.05) |
| ssh-stdio | 4 KiB | 452.61 (264.27–700.60) | 366.26 (232.76–804.45) |
| ssh-stdio | 4 MiB | 749.98 (653.86–872.29) | 731.90 (546.99–2009.58) |

## Session acquisition without GC

Milliseconds: median (minimum–maximum). Selected named roots require narrowing and SSH adds an acknowledged selection command.

| Transport | Payload | Snapshot | Selected |
| --- | ---: | ---: | ---: |
| local | 4 KiB | 6.46 (4.36–9.18) | 6.83 (4.21–8.82) |
| local | 4 MiB | 10.61 (5.98–17.67) | 9.43 (6.69–10.54) |
| ssh-stdio | 4 KiB | 5.18 (3.83–12.12) | 8.46 (6.71–12.73) |
| ssh-stdio | 4 MiB | 8.16 (6.54–9.30) | 10.05 (8.42–16.33) |

## Collection while a stream remains open

Every selected case removed all 16 unrelated logical objects; every snapshot case removed zero. Neither policy reduced pack bytes while open. Selected 4 KiB cases increased pack-file lengths from 4,199,760 to 4,203,946 bytes (+4,186 bytes); selected 4 MiB cases stayed at 8,390,752 bytes. These exact byte outcomes repeated across both transports and all repetitions. Logical cleanup therefore does not yet relieve physical storage pressure in this fixture.

All 96 cases verified stream and destination bytes, retained the original named-root revision after root removal, and reclaimed every source pack after releasing the session. Selected cases with GC then removed the final one logical object; the other cases removed all 17.

GC milliseconds, median. Selected collection performs logical deletion; snapshot collection retains everything, so this is not equal work.

| Transport | Payload | Snapshot GC | Selected GC |
| --- | ---: | ---: | ---: |
| local | 4 KiB | 18.06 | 115.52 |
| local | 4 MiB | 31.90 | 155.16 |
| ssh-stdio | 4 KiB | 22.32 | 143.34 |
| ssh-stdio | 4 MiB | 20.13 | 109.01 |

Copies after the controlled GC overlap also varied widely:

| Transport | Payload | Snapshot copy ms | Selected copy ms |
| --- | ---: | ---: | ---: |
| local | 4 KiB | 251.58 (137.40–1659.68) | 247.40 (204.80–707.82) |
| local | 4 MiB | 737.33 (481.20–2191.16) | 812.31 (650.91–1175.10) |
| ssh-stdio | 4 KiB | 344.21 (187.65–1515.00) | 461.73 (206.50–695.31) |
| ssh-stdio | 4 MiB | 747.45 (453.57–2119.83) | 888.22 (454.58–1435.10) |

The later physical-narrowing experiment was removed because of its GC cost; see
the [batching-only follow-up](2026-09-13-batching-only.md) for the retained change.

## Follow-up: why physical retention remains broad

Code inspection traced the behavior to three places:

- `RetentionHold::retain_only` in `src/repository.rs` replaces the logical scope
  with selected closures but copies the complete snapshot payload catalog into
  the replacement pin.
- `PackedChunks::payload_pin_mark` in `src/blob/pack.rs` collects those catalogs
  without distinguishing logical scopes. `mark_catalog_payloads` protects every
  pack and manifest referenced by their base shards and runs, including historical
  representations that the current catalog has replaced.
- `collection_plan_with_recovery` already computes spilling live-payload and
  live-chunk sets from the retained logical graph. Those sets classify stale
  identities, but are not supplied to historical-catalog physical marking.

Simply dropping the catalog pin is unsafe. Local transfer payload, proof and
chunk access still uses ordinary backend reads; SSH payload, proof, batching
and path discovery similarly depend on the retained view. Scoped object readers
already freeze and pin physical locations, but applying that approach to every
transfer path would require a broader change.

The smaller candidate is to preserve historical catalog metadata while limiting
its payload marking for closure-scoped holds to identities proven live by GC.
That can reuse GC's existing graph traversal instead of adding eager transfer
discovery or a new public session API. It must retain historical locations of
live chunks, not just their replacement locations in the current catalog.
Snapshot holds, staging protection and explicit catalog resources must retain
their existing conservative behavior. Changed pins must invalidate or defer the
filtered mark through the existing inventory/deletion-claim checks. Emergency
sweep before logical publication also needs explicit coverage. This is a design
candidate from the initial inspection. The subsequent implementation and its
[before/after measurements](2026-09-13-scoped-catalog-gc.md) are recorded separately.

Pack granularity limits the potential saving: a historical pack containing any
needed selected chunk must remain available even if most of its bytes are dead.
The local target is 4 MiB. Regression coverage should explicitly construct both
separate garbage-only packs and mixed selected/garbage packs rather than rely on
incidental chunk boundaries. It should also cover delayed reads, proof and batch
reads, another repository handle running GC, changing pins, and final release.
The permanent transfer benchmark can then measure the physical saving and cost.

## Reproduction and evidence

```sh
benchmark run transfer-holds
benchmark all --suites transfer-holds --repetitions 6 --output /tmp/transfer-holds
```

This run built with `cargo bench --offline --features ssh,experimental --bench transfer_holds --no-run -j 2`, copied the reported executable to `/tmp/casita-transfer-holds-bin/transfer_holds`, and ran:

```sh
python3 -m benchmarks.cli all --suites transfer-holds --repetitions 6 \
  --bin-dir /tmp/casita-transfer-holds-bin \
  --output /tmp/casita-transfer-holds-2026-09-13
```

[Raw samples, environment, completion ledger and binary hash](2026-09-13-transfer-holds.json). The permanent runner rejects incomplete matrices and failed correctness gates, retains samples and alternates policy order. `benchmark all` includes the target. All 227 benchmark Python tests passed, along with all-feature/all-target Clippy (`-D warnings`) and formatting checks.
