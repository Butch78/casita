# Reuse of protected catalog lookup state

Baseline: Casita `2271193`, with only the new benchmark probe added. Patched: the implementation committed with this report. Both used the same Cargo.lock and Rust toolchain, with native and experimental features in the unoptimized test profile.

This is a Linux debug microbenchmark, not a macOS FSKit or end-to-end build timing. The development host was not isolated from other work. Three repetitions of 64 admissions per mode; medians below. Fixtures contain a real packed sentinel and production-format catalog history. Payload reads and exact-byte checks are outside the measured admission interval. Every case checks lease release and the expected delta/run boundary.

| Publications | Baseline stable (µs) | Patched stable (µs) | Ratio |
|---:|---:|---:|---:|
| 8 | 59.680 | 2.393 | 24.9× |
| 1023 | 15754.100 | 4.694 | 3356.6× |
| 1024 | 15931.099 | 4.898 | 3252.9× |
| 1025 | 593.963 | 2.245 | 264.6× |

Cold admission still decodes the catalog. Alternating two catalogs intentionally evicts the single cached view and remains near baseline. At 1,025 publications, inline deltas carry into one run; the benchmark covers both sides of that boundary. No reader lease is retained by the cache.

Raw JSON records contain process output, binary hashes, host metadata, and all samples. The runner captures its own checkout identity for both variants; the baseline binary was built separately from the parent revision with the identical probe.

## Reproduction

Run within the repository development environment. Build the parent plus `benchmark_scoped_catalog` and the patch with the same lockfile and flags; copy each executable before rebuilding the other variant.

```sh
cargo test -p casita --lib --no-default-features --features native,experimental --no-run --message-format=json
# Select the casita compiler-artifact executable from the JSON output.
benchmark run scoped-catalog --profile standard --repetitions 3 --probe-binary /path/to/copied-test-binary --no-build --output results.json
```

The ordinary `benchmark run scoped-catalog --profile standard --output results.json` builds a release probe. `benchmark all` includes its bounded smoke configuration. Release and native FSKit follow-up measurements remain outstanding.

## Validation

- 53 catalog-related Rust tests passed, including protected-reader visibility and crash recovery.
- `cargo clippy --all-features --all-targets -- -D warnings` passed.
- 24 benchmark CLI/runner tests passed.
- Formatting and whitespace checks passed.

Lockfile SHA-256: `fa5510227287fb5a664d8fbef9fd9bf6ba98f5e057165c6d5bd33104239f6117`.
Toolchain: `rustc 1.96.0 (ac68faa20 2026-05-25)`.

## Growing pack indexes

The follow-up adds 64 distinct real chunks per publication (512 bytes each), instead of only growing manifest history. It discovers the first catalog carry and measures both the immediately preceding catalog and the carried catalog. In this fixture, the byte limit triggers at publication 276: 275 inline deltas become one run. The selected catalogs contain 17,601 and 17,665 chunks, including the sentinel.

Both variants use the identical updated probe, with three repetitions of eight admissions per mode, in the same Linux debug profile. Baseline production code is `2271193`; patched production code is `63bb012`. Payload checks and newest-chunk visibility checks run outside admission timing. The newest chunk must be absent from the older snapshot, and all protection leases must be released.

| Fixture | Mode | Baseline (ms/open) | Patched (ms/open) |
|---|---|---:|---:|
| packs-below | cold | 1308.643523 | 1332.458233 |
| packs-below | stable | 1260.614697 | 0.024637 |
| packs-below | alternating | 1233.699400 | 1311.721627 |
| packs-above | cold | 53.150818 | 51.930304 |
| packs-above | stable | 52.591450 | 0.004156 |
| packs-above | alternating | 648.966372 | 680.609927 |

The cache removes repeated replay with a growing pack index too. It does not repair cold replay: before the carry, opening the catalog still takes about 1.3 seconds in this debug build. Alternating two catalogs keeps evicting the single cached view. This supports keeping the cache as a focused fix and investigating replay work separately; these timings do not establish release or end-to-end speedups.

```sh
benchmark run scoped-catalog --cases packs-below packs-above --iterations 8 --repetitions 3 \
  --probe-binary /path/to/copied-test-binary --no-build --output growing-results.json
```

The growing cases run by default, including in `benchmark all`. Use `--cases manifests` to reproduce the original manifest-only cases. Raw results are in `growing-baseline.json` and `growing-patched.json`.

## Native FSKit check

On the M4 Pro Mini running macOS 27.0 (26A428), the patched extension mounted successfully without renewed manual enablement. Setup reported that it could not access automatic activation settings, but this was not proof that the extension was disabled. A real mount and completed build confirmed approval was sufficient.

The native comparison uses Obrador's existing pinned Casita `d19901dac63bd47e9ebc035456bc41d9a0c91e08`, with only the cache changes in `pack.rs` and `pack/read.rs` applied to the replacement FSKit app. The release extension was built on the separate macOS 26 build host. Nix, the Obrador plugin, and the Mini's existing repository and toolchain inputs are unchanged. The patched extension binary SHA-256 is `7caaa87aabb43f2d2502b8cb2cacb0cc626f20be0131c4b3d946eb5d72f5af37`.

The workload rebuilds the existing nixpkgs hello derivation with substitution disabled, one build job, and two cores. `--rebuild` checks the output against the existing locally built output. The physical `/nix/store` remains absent. Unpatched wall times were 67.51 s and 68.62 s. The first patched run took 73.33 s, including first launch after replacing the registered extension. The subsequent patched rebuild took 56.50 s. That is faster than both baseline runs, but two runs per variant and different first-launch conditions are insufficient for a robust speedup estimate. The especially slow copied-repository case has not been rerun. The growing-index microbenchmark remains the controlled evidence for avoiding repeated replay. Raw native process logs accompany this report.

```sh
/usr/bin/time -p obrador --option max-jobs 1 --option cores 2 build \
  --option substituters '' --no-link --rebuild \
  '/nix/store/gsnnaglyb9vpvqcgw3i4czy3n892smms-hello-2.12.3.drv^out'
```

This command requires the existing seeded repository and matching local output. The patched test used an extracted copy of the same runtime launcher with only `OBRADOR_FSKIT_APP` changed, after explicit registration of the new app; the baseline used the packaged executable. Startup and registration conditions therefore differ on the first patched run.
