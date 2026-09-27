# Cached process-reader inventories in Obrador

A process-shared cache retains the decoded reader inventory while its file identity is unchanged. Every lookup opens the current path under the ledger lock and compares device, inode, length, modification time and change time. Keeping the cached file open prevents inode reuse. Atomic publication refreshes the cache; foreign publication, corruption and removal invalidate it. The cache holds no reader-owner lock. Disk format, write encoding/checksums, durable write protection and remote fallback remain unchanged.

The permanent `obrador-reads` suite remains registered in `benchmarks/manifest.json` and included in `benchmark all`. Its standard matrix covers 12/100 paths, one/eight workers and GC off/on. All 16 accepted cases passed exact-payload, registered-path, shared-DAG-reference and before/after-GC correctness checks: 14,400 reads. Four contaminated attempts were retained and excluded.

| Paths | Workers | GC | Durable p95 (ms) | Process p95 (ms) |
|---:|---:|:---:|---:|---:|
| 12 | 1 | off | 11.655 | 1.041 |
| 12 | 1 | on | 13.950 | 5.281 |
| 12 | 8 | off | 20.727 | 5.658 |
| 12 | 8 | on | 27.160 | 9.988 |
| 100 | 1 | off | 8.628 | 1.207 |
| 100 | 1 | on | 20.874 | 1.752 |
| 100 | 8 | off | 12.211 | 16.689 |
| 100 | 8 | on | 25.991 | 17.884 |

The matrix has one repetition per case. Process protection is faster in seven of eight pairs, but remains slower in the 100-path/eight-worker GC-off pair. It does not establish that the previous regression is eliminated. Full inventory serialization and hashing still occur on writes.

Both binaries use the same Casita snapshot, changing only retained-reader admission to durable for the control. Obrador implementation and Cargo.lock are preserved. The snapshot is based on `f5d3f14` plus the cache patch, recorded with per-file hashes in the [raw matrix report](2026-09-09-obrador-reader-cache.json). It predates the separate admission/publication changes in `125ae56`; these timings do not measure that combined tree. Original baseline binaries were preserved and their hashes verified.

Before benchmarking, pin protocol tests and the full all-features Rust suite passed, including root replacement with concurrent GC, readers surviving session/repository/runtime drop, process crashes releasing reader protection while durable writes remain protected, and remote API fallback. New cache tests exercise independent handles, foreign atomic publication, same-inode corruption and removal. After rebasing onto `125ae56`, the full all-features suite and formatting passed again; all 192 Python harness tests completed successfully, with two expected skips.

Each attempt requires ten seconds without detected compiler/Nix/Casita jobs and no more than 5% external user-process CPU, followed by the same checks throughout the attempt. Kernel filesystem-worker CPU is recorded separately. These gates do not isolate the hardware or eliminate external disk activity.

```sh
devenv shell python3 -m benchmarks.cli run obrador-reads \
  --reuse-build-report benchmarks/reports/2026-09-09-obrador-reader-profiles.json \
  --rebuild-casita --casita-source /path/to/cache-checkout \
  --profile standard --paths 12,100 --workers 1,8 --require-quiet-host \
  --output /tmp/casita-obrador-reader-cache.json
```

Rebuilding requires the retained copied workspaces referenced in the prior report. On a new host, use `--obrador-source` with the exact Obrador checkout instead; see the [runner documentation](../obrador-reads.md). The cache patch is commit `207740a`; to reproduce the measured Casita tree, apply that patch to `f5d3f14` rather than including the intervening upstream admission change.

After integrating the subsequent shared Linux/macOS journal changes from `3265d22`, Linux validation passed again: 67 pin tests (five ignored), all 16 retained-reader/publication/application/S3 integration tests, the 192-test Python suite (two skips), and Clippy with all features and targets. This final merge is `1da8e43`. The cache patch was not independently tested on macOS during this investigation.

A supplemental five-repetition run at 100 paths/eight workers was stopped after persistent competing compiler activity. It produced no accepted cases; one completed attempt was contaminated and excluded. Its [incomplete raw report](2026-09-09-obrador-reader-cache-repeat-incomplete.json) retains that attempt, activity samples, binary hashes and source provenance. This is not repeat confirmation of the matrix result.

```sh
python3 -m benchmarks.cli run obrador-reads \
  --reuse-build-report /tmp/casita-obrador-reader-cache.json \
  --profile standard --paths 100 --workers 8 --repetitions 5 \
  --require-quiet-host --output /tmp/casita-obrador-reader-cache-repeat.json
```
