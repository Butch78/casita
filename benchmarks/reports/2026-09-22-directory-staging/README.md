# Bounded filesystem directory staging

Baseline: `9b5ded9`. Measured candidate: `9cb2dd3c`, the bounded directory-staging change before its PR rebase onto upstream `8b5220a9`. Timings describe those retained binaries; they were not rerun after the rebase. Both immutable binaries were built with `cargo build --features cli --bin casita`, Rust 1.96.0, development profile. Raw output, traces, fixture trees, restored trees, and hashed binaries are retained under `/tmp/casita-directory-staging/`. [The compact results](summary.json) retain every sample, binary hash, and raw-report location.

The host was shared with other workloads. Rust builds and Rust tests from this investigation did not overlap measured imports. The initial sequential sweep was noticeably slower than the later sequential control, so the late controls below are the more conservative comparison. These are observations, not stable latency budgets.

## Late controls

| Workload | Sequential untraced seconds | Concurrent untraced seconds | Sequential traced seconds | Concurrent traced seconds | Journal flushes before / after |
|---|---:|---:|---:|---:|---:|
| 1024 child directories, median of 3 | 2.323 | 0.923 | 2.634 | 1.145 | 1047 / 92 |
| Riff, one sample each | 3.245 | 2.012 | 3.466 | 1.963 | 1098 / 130 |

Riff contains 3,060 regular files and 1,416 directories. Every warm trace had 3,060 hits, zero misses, and zero blob stages. The late controls ran sequential 1024, sequential Riff, concurrent 1024, then concurrent Riff.

## Complete boundary sweep

Each child contains a distinct 64-byte file. Counts exclude the root; walk pages count both files and directories. The 2047/2048/2049 cases cross the forced-reread publication boundary; 4095/4096/4097 cross the cached publication boundary. These cases are permanent in `filesystem-reuse --profile standard`, registered in the manifest and run by `benchmark all`.

| Children | Sequential warm seconds | Concurrent warm seconds | Sequential traced seconds | Concurrent traced seconds | Journal flushes before / after |
|---:|---:|---:|---:|---:|---:|
| 1 | 0.083 | 0.069 | 0.073 | 0.065 | 12 / 11 |
| 14 | 0.108 | 0.070 | 0.110 | 0.077 | 25 / 11 |
| 15 | 0.129 | 0.073 | 0.113 | 0.080 | 26 / 11 |
| 16 | 0.126 | 0.075 | 0.170 | 0.079 | 27 / 12 |
| 17 | 0.121 | 0.078 | 0.109 | 0.089 | 28 / 12 |
| 64 | 0.241 | 0.101 | 0.276 | 0.122 | 75 / 15 |
| 511 | 1.666 | 0.395 | 1.806 | 0.757 | 531 / 53 |
| 512 | 5.041 | 0.403 | 6.163 | 0.661 | 533 / 55 |
| 513 | 5.229 | 0.405 | 5.145 | 0.660 | 535 / 56 |
| 1024 | 6.567 | 0.941 | 6.854 | 1.269 | 1047 / 92 |
| 2047 | 19.366 | 1.498 | 16.560 | 2.179 | 2073 / 164 |
| 2048 | 14.570 | 4.491 | 8.974 | 2.580 | 2076 / 167 |
| 2049 | 10.856 | 1.455 | 8.486 | 2.090 | 2078 / 168 |
| 4095 | 8.152 | 2.744 | 10.972 | 4.171 | 4134 / 315 |
| 4096 | 9.024 | 2.810 | 12.750 | 4.201 | 4137 / 319 |
| 4097 | 13.992 | 2.527 | 10.889 | 3.907 | 4140 / 320 |

## Initial Riff repetitions

| Operation | Sequential median seconds (range) | Concurrent median seconds (range) |
|---|---:|---:|
| warm | 10.405 (5.214 to 14.259) | 1.489 (1.452 to 1.720) |
| warm-traced | 10.578 (5.833 to 11.005) | 1.754 (1.742 to 1.972) |
| rehash-traced | 20.839 (20.617 to 24.720) | 10.353 (10.131 to 10.546) |

All three sequential warm traces had 1,098 journal flushes; all three concurrent traces had 130. The large difference between early and late sequential timings is why the initial median is not used alone as the speedup claim.

## Correctness and safety

All eight benchmark runs completed every gate: unchanged source inventory, byte-for-byte restoration with paths, executable bits and symlink text, canonical identity across prime/warm/rehash imports, and full warm file-cache reuse. An additional comparison verified identical canonical keys across both binaries for every generated count and Riff, including all controls.

The implementation constructs directory identities in post-order, retains at most one walk page of completed directories, and uses 16 ordered buffered staging futures. Files still precede directories in the publication queue, and completed directories retain their original post-order. Batch limits are unchanged. The existing `stage_directory` write scope, durable pin protection, cancellation-safe backend operations, and GC coordination are unchanged.

Validation passed:

- `cargo test --all-features --lib directory_staging_is_bounded_ordered_and_cancellable`: paused first write, full 16-write window, out-of-order completion, page crossing, canonical identity, checkout, batch limits 1 and 17, failure propagation, and cancellation cleanup.
- `cargo test --all-features --lib repository::`: 98 passed, 4 ignored benchmark cases.
- `cargo test --all-features --lib filesystem::`: 20 passed, 1 ignored benchmark case.
- `cargo test --all-features --lib metadata::pins::`: 81 passed, 6 ignored benchmark/worker/disposable-full-volume cases.
- Selected integration suites: `rooted_filesystem`, `publication_cancellation`, `online_collection`, `online_holds_contention`, `mutation_sessions`, `repository_workflows`, and `online_pin_ledger`, all passed. The contention fixture now stages 17 directories alongside readers and GC.
- `cargo clippy --all-features --all-targets -- -D warnings`, `cargo fmt --all -- --check`, and `git diff --check` passed.
- `python3 -m unittest discover -s benchmarks -p 'test_*.py'`: 354 tests run, 2 existing skips, no failures.

## Remaining costs

Concurrent staging removes most per-directory durability round trips, but still stages every directory payload. In the three initial concurrent Riff traces, metadata `state.commit` took 0.239 to 0.253s, repository opening 0.240 to 0.334s, and file-cache recall 0.157 to 0.162s. Journal flush phase totals remained 0.208 to 0.378s across 130 events. These spans and phases can overlap and are not an additive wall-clock breakdown.

The summed directory-stage spans increase under concurrency because they include overlapping waits; that is not a regression in elapsed staging time. Publication and repository opening are now useful next profiling targets, along with the remaining directory write work. Prefer a quiet-host confirmation and targeted profiling before increasing concurrency or changing durability behavior.

## Reproduce

For a fresh comparison on the rebased PR, build `BEFORE` from its benchmark-only commit `eef923f9` and `AFTER` from the PR tip, with the same toolchain and build profile. The archived measurements above instead used the original pre-rebase revisions and retained binaries. Run builds and tests outside the timing window. `RIFF_SOURCE` is a read-only retained Cargo target; the measured source here was `/tmp/cargo-casita-project.e8LVAU/target`.

```sh
benchmark run filesystem-reuse --casita-bin "$BEFORE" --profile standard --output /tmp/fs-before.json
benchmark run filesystem-reuse --casita-bin "$AFTER" --profile standard --output /tmp/fs-after.json
benchmark run filesystem-reuse --casita-bin "$BEFORE" --source "$RIFF_SOURCE" --repetitions 3 --output /tmp/riff-before.json
benchmark run filesystem-reuse --casita-bin "$AFTER" --source "$RIFF_SOURCE" --repetitions 3 --output /tmp/riff-after.json
# Repeat after the initial sweeps to check host drift:
benchmark run filesystem-reuse --casita-bin "$BEFORE" --directories 1024 --repetitions 3 --output /tmp/control-before.json
benchmark run filesystem-reuse --casita-bin "$BEFORE" --source "$RIFF_SOURCE" --output /tmp/riff-control-before.json
benchmark run filesystem-reuse --casita-bin "$AFTER" --directories 1024 --repetitions 3 --output /tmp/control-after.json
benchmark run filesystem-reuse --casita-bin "$AFTER" --source "$RIFF_SOURCE" --output /tmp/riff-control-after.json
```
